use std::io::Write as _;
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use crate::core::backend::Backend;
use crate::core::config::{ClusterConfig, VisualConfig, VmDef, WindowTarget, par_each_vm};
use crate::game::compositor;
use crate::ui::cli::ScreenshotBackend;

#[derive(Clone, Debug)]
pub struct ScreenshotOptions {
    pub backend: ScreenshotBackend,
    pub validator: Option<String>,
    pub golden: Option<VisualConfig>,
    pub scene: Option<String>,
    pub run_label: Option<String>,
    pub window_target: Option<WindowTarget>,
    pub allow_vnc_input: bool,
}

impl Default for ScreenshotOptions {
    fn default() -> Self {
        Self {
            backend: ScreenshotBackend::Grim,
            validator: None,
            golden: None,
            scene: None,
            run_label: None,
            window_target: None,
            allow_vnc_input: false,
        }
    }
}

/// Capture a screenshot from an instance using grim (Wayland screenshot tool).
pub async fn screenshot_wayland_vm(
    backend: &Backend,
    vm: &VmDef,
    output_dir: &Path,
    target: Option<&WindowTarget>,
) -> anyhow::Result<PathBuf> {
    compositor::wait_until_ready(backend, vm, target, compositor::default_ready_timeout()).await?;
    let remote_path = "/tmp/screenshot.png";
    let cmd = format!(
        "export XDG_RUNTIME_DIR=/tmp/runtime-$(whoami); \
         export WAYLAND_DISPLAY=wayland-1; \
         grim {remote_path} 2>/dev/null && echo OK || echo FAIL"
    );
    let result = backend.run_cmd(&vm.ip, &cmd).await;
    if !result.stdout.trim().contains("OK") {
        anyhow::bail!("{}: grim returned no output (is sway running?)", vm.name);
    }

    // Download via upload (rsync reverse direction)
    let local_path = output_dir.join(format!("{}.png", vm.name));
    let sources = [Path::new(remote_path)];
    backend
        .upload(&sources, &vm.ip, local_path.to_str().unwrap_or("."))
        .await?;

    Ok(local_path)
}

/// Start wayvnc inside a VM.
///
/// Plain screenshot flows keep `--disable-input` as defense in depth. Scene
/// scripting can request input-enabled wayvnc explicitly; the bridge is
/// host-private, but we still default to the tighter mode unless a caller
/// opts in for scripted navigation.
pub fn wayvnc_start_script(vm_user: &str, allow_input: bool) -> String {
    let disable_flag = if allow_input { "" } else { "--disable-input " };
    format!(
        r#"
export XDG_RUNTIME_DIR=/tmp/runtime-{vm_user}
mkdir -p "$XDG_RUNTIME_DIR"
chmod 700 "$XDG_RUNTIME_DIR" 2>/dev/null || true
export WAYLAND_DISPLAY=wayland-1
if ! pgrep -x sway >/dev/null; then
    echo FAIL
    echo "sway is not running; VNC capture requires a sway/sway-gpu session"
    if pgrep -x weston >/dev/null; then
        echo "weston is running instead"
    fi
    exit 1
fi
export SWAYSOCK="$(find "$XDG_RUNTIME_DIR" -maxdepth 1 -type s -name 'sway-ipc.*.sock' 2>/dev/null | head -n1)"
if [ -z "$SWAYSOCK" ] || ! swaymsg -r -t get_outputs >/dev/null 2>&1; then
    echo FAIL
    echo "sway IPC is not ready"
    exit 1
fi
current_wayvnc="$(pgrep -af '(^|/)wayvnc( |$)' || true)"
if [ -n "$current_wayvnc" ]; then
    if printf '%s\n' "$current_wayvnc" | grep -q -- '--disable-input'; then
        current_input_mode="disabled"
    else
        current_input_mode="enabled"
    fi
    if printf '%s\n' "$current_wayvnc" | grep -q -- '--disable-resizing'; then
        current_resizing_mode="disabled"
    else
        current_resizing_mode="enabled"
    fi
    desired_input_mode="{desired_input_mode}"
    if [ "$current_input_mode" != "$desired_input_mode" ] || [ "$current_resizing_mode" != "disabled" ]; then
        pkill -x wayvnc 2>/dev/null || true
        sleep 1
    fi
fi
if ! pgrep -x wayvnc >/dev/null; then
    rm -f "$XDG_RUNTIME_DIR/wayvnc.log"
    nohup env XDG_RUNTIME_DIR="$XDG_RUNTIME_DIR" WAYLAND_DISPLAY="$WAYLAND_DISPLAY" \
        wayvnc {disable_flag}--disable-resizing --log-level=info 0.0.0.0 5900 >"$XDG_RUNTIME_DIR/wayvnc.log" 2>&1 &
    sleep 2
fi
if pgrep -x wayvnc >/dev/null; then
    echo OK
else
    echo FAIL
    if [ -f "$XDG_RUNTIME_DIR/wayvnc.log" ]; then
        echo "--- wayvnc.log ---"
        tail -n 40 "$XDG_RUNTIME_DIR/wayvnc.log"
    fi
fi
"#,
        desired_input_mode = if allow_input { "enabled" } else { "disabled" }
    )
}

pub async fn ensure_wayvnc(
    backend: &Backend,
    vm: &VmDef,
    vm_user: &str,
    allow_input: bool,
) -> anyhow::Result<()> {
    let cmd = wayvnc_start_script(vm_user, allow_input);
    let result = backend.run_cmd(&vm.ip, &cmd).await;
    if result.stdout.trim().contains("OK") {
        compositor::wait_until_ready(backend, vm, None, compositor::default_ready_timeout()).await
    } else {
        anyhow::bail!(
            "{}: wayvnc failed to start; VNC screenshots require an active sway/sway-gpu session\nstdout:\n{}\nstderr:\n{}",
            vm.name,
            result.stdout.trim(),
            result.stderr.trim()
        );
    }
}

pub async fn ensure_wayvnc_all<S>(
    config: &ClusterConfig<S>,
    allow_input: bool,
) -> anyhow::Result<()> {
    let backend = config.backend.clone();
    let vm_user = config.vm_user.clone();
    let results = par_each_vm(&config.vms, |vm| {
        let backend = backend.clone();
        let vm_user = vm_user.clone();
        async move {
            let name = vm.name.to_string();
            (
                name,
                ensure_wayvnc(&backend, &vm, &vm_user, allow_input).await,
            )
        }
    })
    .await?;

    let failures: Vec<String> = results
        .into_iter()
        .filter_map(|(name, result)| result.err().map(|e| format!("{name}: {e}")))
        .collect();
    if failures.is_empty() {
        Ok(())
    } else {
        anyhow::bail!("{}", failures.join("\n"));
    }
}

pub async fn screenshot_vnc_vm(
    backend: &Backend,
    vm: &VmDef,
    output_dir: &Path,
    target: Option<&WindowTarget>,
) -> anyhow::Result<PathBuf> {
    compositor::wait_until_ready(backend, vm, target, compositor::default_ready_timeout()).await?;
    let local_path = output_dir.join(format!("{}.png", vm.name));
    let ip = vm.ip.to_string();
    let path = local_path.clone();
    tokio::task::spawn_blocking(move || capture_vnc_framebuffer(&ip, &path))
        .await
        .map_err(|e| anyhow::anyhow!("{}: VNC screenshot task failed: {e}", vm.name))??;
    Ok(local_path)
}

pub async fn screenshot_vm(
    backend: &Backend,
    vm: &VmDef,
    output_dir: &Path,
    screenshot_backend: ScreenshotBackend,
    target: Option<&WindowTarget>,
) -> anyhow::Result<PathBuf> {
    match screenshot_backend {
        ScreenshotBackend::Grim => screenshot_wayland_vm(backend, vm, output_dir, target).await,
        ScreenshotBackend::Vnc => screenshot_vnc_vm(backend, vm, output_dir, target).await,
    }
}

const VNC_CAPTURE_TIMEOUT: Duration = Duration::from_secs(10);
const VNC_CAPTURE_POLL_INTERVAL: Duration = Duration::from_millis(20);
const VNC_CAPTURE_MAX_ATTEMPTS: usize = 5;
const NONBLANK_ROW_BAND_HEIGHT: usize = 32;

fn capture_vnc_framebuffer(ip: &str, path: &Path) -> anyhow::Result<()> {
    let stream = TcpStream::connect((ip, 5900))?;
    stream.set_read_timeout(Some(VNC_CAPTURE_TIMEOUT))?;
    stream.set_write_timeout(Some(VNC_CAPTURE_TIMEOUT))?;

    let mut client = vnc::Client::from_tcp_stream(stream, true, |methods| {
        methods
            .iter()
            .find(|method| matches!(method, vnc::client::AuthMethod::None))
            .map(|_| vnc::client::AuthChoice::None)
    })
    .map_err(|e| anyhow::anyhow!("connect to VNC {ip}: {e}"))?;

    let (width, height) = client.size();
    let desired_format = vnc::PixelFormat {
        bits_per_pixel: 32,
        depth: 24,
        big_endian: false,
        true_colour: true,
        red_max: 255,
        green_max: 255,
        blue_max: 255,
        red_shift: 16,
        green_shift: 8,
        blue_shift: 0,
    };
    client
        .set_format(desired_format)
        .map_err(|e| anyhow::anyhow!("set VNC pixel format: {e}"))?;
    // `vnc` 0.4.0 supports ZRLE and Raw, but not Tight. Prefer ZRLE to reduce
    // large full-frame transfers while keeping Raw as a fallback.
    client
        .set_encodings(&[vnc::Encoding::Zrle, vnc::Encoding::Raw])
        .map_err(|e| anyhow::anyhow!("set VNC encodings: {e}"))?;

    let rect = vnc::Rect {
        left: 0,
        top: 0,
        width,
        height,
    };
    let mut state = VncFramebufferState::new(width, height);
    let mut update_requests = 0usize;
    request_vnc_update(&mut client, rect, false, &state, &mut update_requests)?;

    let deadline = Instant::now() + VNC_CAPTURE_TIMEOUT;
    while Instant::now() < deadline {
        let events: Vec<_> = client.poll_iter().collect();
        for event in &events {
            match event {
                vnc::client::Event::PutPixels(rect, pixels) => {
                    state.blit_pixels(desired_format, *rect, pixels)?;
                }
                vnc::client::Event::EndOfFrame => {
                    state.end_of_frame_events += 1;
                    if state.is_complete() {
                        write_vnc_framebuffer(path, &state.rgba, width, height)?;
                        validate_image_nonblank(path)?;
                        return Ok(());
                    }
                    if update_requests >= VNC_CAPTURE_MAX_ATTEMPTS {
                        return Err(state.incomplete_frame_error(update_requests));
                    }
                    request_vnc_update(&mut client, rect, true, &state, &mut update_requests)?;
                }
                vnc::client::Event::Disconnected(error) => {
                    state.last_rfb_error = Some(match error {
                        Some(error) => error.to_string(),
                        None => "peer disconnected".to_string(),
                    });
                    anyhow::bail!("{}", state.disconnected_error(update_requests));
                }
                vnc::client::Event::Resize(new_width, new_height) => {
                    anyhow::bail!(
                        "VNC framebuffer resized during capture from {}x{} to {}x{}",
                        width,
                        height,
                        new_width,
                        new_height
                    );
                }
                vnc::client::Event::CopyPixels { src, dst } => {
                    anyhow::bail!(
                        "VNC server sent unsupported CopyRect update from ({}, {}) to ({}, {}) sized {}x{}",
                        src.left,
                        src.top,
                        dst.left,
                        dst.top,
                        dst.width,
                        dst.height
                    );
                }
                _ => {}
            }
        }
        if events.is_empty() {
            std::thread::sleep(VNC_CAPTURE_POLL_INTERVAL);
        }
    }

    anyhow::bail!("{}", state.timeout_error(update_requests));
}

fn blit_vnc_pixels(
    rgba: &mut [u8],
    framebuffer_width: u16,
    format: vnc::PixelFormat,
    rect: vnc::Rect,
    pixels: &[u8],
) -> anyhow::Result<()> {
    let bytes_per_pixel = usize::from(format.bits_per_pixel / 8);
    let rect_width = usize::from(rect.width);
    let rect_height = usize::from(rect.height);
    let expected = rect_width * rect_height * bytes_per_pixel;
    if pixels.len() < expected {
        anyhow::bail!(
            "short VNC pixel buffer: got {}, expected {expected}",
            pixels.len()
        );
    }

    for y in 0..rect_height {
        for x in 0..rect_width {
            let src = (y * rect_width + x) * bytes_per_pixel;
            let pixel = read_pixel_value(&pixels[src..src + bytes_per_pixel], format);
            let dst_x = usize::from(rect.left) + x;
            let dst_y = usize::from(rect.top) + y;
            let dst = (dst_y * usize::from(framebuffer_width) + dst_x) * 4;
            rgba[dst] = channel(pixel, format.red_max, format.red_shift);
            rgba[dst + 1] = channel(pixel, format.green_max, format.green_shift);
            rgba[dst + 2] = channel(pixel, format.blue_max, format.blue_shift);
            rgba[dst + 3] = 255;
        }
    }
    Ok(())
}

fn write_vnc_framebuffer(path: &Path, rgba: &[u8], width: u16, height: u16) -> anyhow::Result<()> {
    image::save_buffer(
        path,
        rgba,
        width as u32,
        height as u32,
        image::ColorType::Rgba8,
    )?;
    Ok(())
}

fn request_vnc_update(
    client: &mut vnc::Client,
    rect: vnc::Rect,
    incremental: bool,
    state: &VncFramebufferState,
    update_requests: &mut usize,
) -> anyhow::Result<()> {
    client.request_update(rect, incremental).map_err(|e| {
        anyhow::anyhow!("{}: {e}", state.request_error_prefix(*update_requests + 1))
    })?;
    *update_requests += 1;
    Ok(())
}

fn read_pixel_value(bytes: &[u8], format: vnc::PixelFormat) -> u32 {
    let mut value = 0u32;
    if format.big_endian {
        for byte in bytes {
            value = (value << 8) | u32::from(*byte);
        }
    } else {
        for (shift, byte) in bytes.iter().enumerate() {
            value |= u32::from(*byte) << (shift * 8);
        }
    }
    value
}

fn channel(pixel: u32, max: u16, shift: u8) -> u8 {
    if max == 0 {
        return 0;
    }
    let raw = (pixel >> shift) & u32::from(max);
    ((raw * 255) / u32::from(max)) as u8
}

fn validate_image_nonblank(path: &Path) -> anyhow::Result<()> {
    let image = image::open(path)
        .map_err(|e| anyhow::anyhow!("read VNC screenshot {}: {e}", path.display()))?
        .to_rgba8();
    if image.width() == 0 || image.height() == 0 {
        anyhow::bail!("VNC screenshot {} has zero dimensions", path.display());
    }
    let uniform_row_band = detect_uniform_row_band(&image);

    let mut min_luma = u8::MAX;
    let mut max_luma = u8::MIN;
    let mut first = None;
    let mut distinct = 0usize;
    for pixel in image.pixels() {
        let [r, g, b, a] = pixel.0;
        let luma = ((u16::from(r) + u16::from(g) + u16::from(b)) / 3) as u8;
        min_luma = min_luma.min(luma);
        max_luma = max_luma.max(luma);
        if first.is_none() {
            first = Some([r, g, b, a]);
        } else if first != Some([r, g, b, a]) {
            distinct += 1;
            if distinct > 32 && max_luma.saturating_sub(min_luma) > 8 && uniform_row_band.is_none()
            {
                return Ok(());
            }
        }
    }

    if let Some(reason) = uniform_row_band {
        anyhow::bail!(
            "VNC screenshot {} appears blank ({reason}; luma range {}..{}, distinct samples {})",
            path.display(),
            min_luma,
            max_luma,
            distinct
        );
    }

    anyhow::bail!(
        "VNC screenshot {} appears blank (luma range {}..{}, distinct samples {})",
        path.display(),
        min_luma,
        max_luma,
        distinct
    );
}

fn detect_uniform_row_band(image: &image::RgbaImage) -> Option<String> {
    let height = image.height() as usize;
    let width = image.width() as usize;
    if height < NONBLANK_ROW_BAND_HEIGHT || width == 0 {
        return None;
    }

    let first = image.get_pixel(0, 0).0;
    for start_row in 0..=height - NONBLANK_ROW_BAND_HEIGHT {
        let mut all_zero_alpha = true;
        let mut all_first_colour = true;
        'band: for y in start_row..start_row + NONBLANK_ROW_BAND_HEIGHT {
            for x in 0..width {
                let pixel = image.get_pixel(x as u32, y as u32).0;
                if pixel[3] != 0 {
                    all_zero_alpha = false;
                }
                if pixel != first {
                    all_first_colour = false;
                }
                if !all_zero_alpha && !all_first_colour {
                    break 'band;
                }
            }
        }
        if all_zero_alpha {
            return Some(format!(
                "rows {start_row}..{} are entirely zero-alpha",
                start_row + NONBLANK_ROW_BAND_HEIGHT
            ));
        }
        if all_first_colour {
            return Some(format!(
                "rows {start_row}..{} are entirely the first pixel colour",
                start_row + NONBLANK_ROW_BAND_HEIGHT
            ));
        }
    }

    None
}

struct VncFramebufferState {
    rgba: Vec<u8>,
    covered: Vec<bool>,
    width: u16,
    height: u16,
    covered_pixels: usize,
    put_pixels_events: usize,
    end_of_frame_events: usize,
    last_rfb_error: Option<String>,
}

impl VncFramebufferState {
    fn new(width: u16, height: u16) -> Self {
        let pixel_count = width as usize * height as usize;
        Self {
            rgba: vec![0; pixel_count * 4],
            covered: vec![false; pixel_count],
            width,
            height,
            covered_pixels: 0,
            put_pixels_events: 0,
            end_of_frame_events: 0,
            last_rfb_error: None,
        }
    }

    fn blit_pixels(
        &mut self,
        format: vnc::PixelFormat,
        rect: vnc::Rect,
        pixels: &[u8],
    ) -> anyhow::Result<()> {
        if format.bits_per_pixel != 32 {
            anyhow::bail!(
                "VNC pixel format invariant violated: expected 32bpp, got {}bpp",
                format.bits_per_pixel
            );
        }
        self.validate_rect(rect)?;
        blit_vnc_pixels(&mut self.rgba, self.width, format, rect, pixels)?;
        self.mark_coverage(rect);
        self.put_pixels_events += 1;
        Ok(())
    }

    fn is_complete(&self) -> bool {
        self.covered_pixels == self.covered.len()
    }

    fn coverage_percent(&self) -> f64 {
        if self.covered.is_empty() {
            100.0
        } else {
            self.covered_pixels as f64 * 100.0 / self.covered.len() as f64
        }
    }

    fn request_error_prefix(&self, next_attempt: usize) -> String {
        format!(
            "request VNC framebuffer update attempt {next_attempt} with {}",
            self.stats_summary()
        )
    }

    fn timeout_error(&self, update_requests: usize) -> String {
        format!(
            "VNC framebuffer update timed out after {}s with {}",
            VNC_CAPTURE_TIMEOUT.as_secs(),
            self.stats_summary_with_requests(update_requests)
        )
    }

    fn incomplete_frame_error(&self, update_requests: usize) -> anyhow::Error {
        anyhow::anyhow!(
            "VNC framebuffer remained incomplete after {update_requests} update requests: {}",
            self.stats_summary()
        )
    }

    fn disconnected_error(&self, update_requests: usize) -> String {
        format!(
            "VNC disconnected during framebuffer capture after {update_requests} update requests with {}",
            self.stats_summary()
        )
    }

    fn stats_summary_with_requests(&self, update_requests: usize) -> String {
        format!(
            "{}, update requests {update_requests}",
            self.stats_summary()
        )
    }

    fn stats_summary(&self) -> String {
        let last_rfb_error = self.last_rfb_error.as_deref().unwrap_or("none");
        format!(
            "coverage {:.1}% ({}/{} pixels), PutPixels events {}, EndOfFrame events {}, last RFB error {last_rfb_error}",
            self.coverage_percent(),
            self.covered_pixels,
            self.covered.len(),
            self.put_pixels_events,
            self.end_of_frame_events
        )
    }

    fn validate_rect(&self, rect: vnc::Rect) -> anyhow::Result<()> {
        let right = usize::from(rect.left) + usize::from(rect.width);
        let bottom = usize::from(rect.top) + usize::from(rect.height);
        if right > usize::from(self.width) || bottom > usize::from(self.height) {
            anyhow::bail!(
                "VNC rectangle {}x{} at ({}, {}) exceeds framebuffer {}x{}",
                rect.width,
                rect.height,
                rect.left,
                rect.top,
                self.width,
                self.height
            );
        }
        Ok(())
    }

    fn mark_coverage(&mut self, rect: vnc::Rect) {
        let width = usize::from(self.width);
        let top = usize::from(rect.top);
        let left = usize::from(rect.left);
        let bottom = top + usize::from(rect.height);
        let right = left + usize::from(rect.width);
        for y in top..bottom {
            let row_start = y * width;
            for x in left..right {
                let index = row_start + x;
                if !self.covered[index] {
                    self.covered[index] = true;
                    self.covered_pixels += 1;
                }
            }
        }
    }
}

/// Capture screenshots from all instances in parallel.
pub async fn screenshot_all<S>(
    config: &ClusterConfig<S>,
    output_dir: &Path,
    options: &ScreenshotOptions,
) -> anyhow::Result<()> {
    std::fs::create_dir_all(output_dir)?;
    let backend = config.backend.clone();

    println!("==> Capturing {} screenshots...", options.backend);

    if options.backend == ScreenshotBackend::Vnc {
        ensure_wayvnc_all(config, options.allow_vnc_input).await?;
    }

    let results = par_each_vm(&config.vms, |vm| {
        let backend = backend.clone();
        let output_dir = output_dir.to_owned();
        let options = options.clone();
        async move {
            let result = screenshot_vm(
                &backend,
                &vm,
                &output_dir,
                options.backend,
                options.window_target.as_ref(),
            )
            .await;
            match result {
                Ok(path) => (vm.name, Ok(path)),
                Err(e) => (vm.name, Err(e)),
            }
        }
    })
    .await?;

    let mut failures = Vec::new();
    for (name, result) in results {
        match result {
            Ok(path) => {
                println!("  {name}: {}", path.display());
                if let Some(validator) = options.validator.as_deref()
                    && let Err(error) = run_validator(validator, &path, name.as_ref(), options) {
                        failures.push(format!("{name}: {error}"));
                    }
                if let Some(visual) = options.golden.as_ref()
                    && let Err(error) = run_golden_validator(visual, &path, options) {
                        failures.push(format!("{name}: {error}"));
                    }
            }
            Err(e) => {
                eprintln!("  {name}: {e}");
                failures.push(format!("{name}: {e}"));
            }
        }
    }
    if !failures.is_empty() {
        anyhow::bail!("{}", failures.join("\n"));
    }
    println!("==> Done");
    Ok(())
}

/// Capture screenshots on test failure. Called from test.rs.
pub async fn capture_on_failure<S>(
    config: &ClusterConfig<S>,
    output_dir: &Path,
    run_num: u32,
    options: &ScreenshotOptions,
) -> Vec<crate::game::visual::VisualRunResult> {
    let screenshot_dir = output_dir.join(format!("screenshots/run-{run_num}"));
    let options = ScreenshotOptions {
        run_label: Some(format!("run-{run_num}")),
        ..options.clone()
    };
    let mut visual_results = Vec::new();
    if let Err(e) = screenshot_all(config, &screenshot_dir, &options).await {
        eprintln!("  Screenshot capture failed: {e}");
    }
    if let Some(visual) = options.golden.as_ref() {
        for scene in selected_scenes(visual, options.scene.as_deref()) {
            let paths = match visual.diff_path(
                &scene.name,
                options.run_label.as_deref().unwrap_or("manual"),
            ) {
                Ok(paths) => paths,
                Err(error) => {
                    eprintln!("  Visual diff path failed for {}: {error}", scene.name);
                    continue;
                }
            };
            if paths.actual.exists() {
                let engine = crate::game::visual::Engine::new(visual.clone());
                match crate::game::visual::diff_scene(
                    visual,
                    &engine,
                    scene,
                    options.run_label.as_deref().unwrap_or("manual"),
                ) {
                    Ok(result) => visual_results.push(result),
                    Err(error) => eprintln!("  Visual diff failed for {}: {error}", scene.name),
                }
            }
        }
    }
    visual_results
}

#[allow(dead_code)]
pub async fn capture_single_vm<S>(
    config: &ClusterConfig<S>,
    vm: &VmDef,
    output_dir: &Path,
    options: &ScreenshotOptions,
) -> anyhow::Result<(PathBuf, Vec<crate::game::visual::VisualRunResult>)> {
    std::fs::create_dir_all(output_dir)?;
    if options.backend == ScreenshotBackend::Vnc {
        ensure_wayvnc(
            &config.backend,
            vm,
            &config.vm_user,
            options.allow_vnc_input,
        )
        .await?;
    }
    let path = screenshot_vm(
        &config.backend,
        vm,
        output_dir,
        options.backend,
        options.window_target.as_ref(),
    )
    .await?;
    if let Some(validator) = options.validator.as_deref() {
        run_validator(validator, &path, vm.name.as_ref(), options)?;
    }
    let visual_results = if let Some(visual) = options.golden.as_ref() {
        run_golden_validator(visual, &path, options)?
    } else {
        Vec::new()
    };
    Ok((path, visual_results))
}

fn run_validator(
    validator: &str,
    image_path: &Path,
    vm_name: &str,
    options: &ScreenshotOptions,
) -> anyhow::Result<()> {
    let output = Command::new("sh")
        .arg("-c")
        .arg(validator)
        .env("STEAMPIPE_VISUAL_IMAGE", image_path)
        .env("STEAMPIPE_VISUAL_VM", vm_name)
        .env(
            "STEAMPIPE_VISUAL_RUN",
            options.run_label.as_deref().unwrap_or("manual"),
        )
        .env("STEAMPIPE_VISUAL_BACKEND", options.backend.to_string())
        .output()?;

    let stem = image_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("screenshot");
    let stdout_path = image_path.with_file_name(format!("{stem}.validator.stdout.txt"));
    let stderr_path = image_path.with_file_name(format!("{stem}.validator.stderr.txt"));
    std::fs::File::create(stdout_path)?.write_all(&output.stdout)?;
    std::fs::File::create(stderr_path)?.write_all(&output.stderr)?;

    if output.status.success() {
        println!("  {vm_name}: visual validator passed");
        Ok(())
    } else {
        anyhow::bail!(
            "{vm_name}: visual validator failed with status {}",
            output.status
        );
    }
}

fn run_golden_validator(
    visual: &VisualConfig,
    image_path: &Path,
    options: &ScreenshotOptions,
) -> anyhow::Result<Vec<crate::game::visual::VisualRunResult>> {
    let run_label = options.run_label.as_deref().unwrap_or("manual");
    let engine = crate::game::visual::Engine::new(visual.clone());
    let mut failures = Vec::new();
    let mut compared = 0usize;
    let mut written_results = Vec::new();
    for scene in selected_scenes(visual, options.scene.as_deref()) {
        compared += 1;
        let result = engine.compare(image_path, scene)?;
        let written = crate::game::visual::write_result_artifacts(
            visual, scene, run_label, image_path, &result,
        )?;
        println!(
            "  {}: visual scene {} {} (ssim {:.5}, max_delta {}, result {})",
            image_path.display(),
            scene.name,
            if result.passed { "passed" } else { "failed" },
            result.ssim,
            result.max_delta,
            written.result_path.display()
        );
        written_results.push(written);
        if !result.passed {
            failures.push(format!(
                "{} failed: ssim {:.5} >= {:.5}, max_delta {} <= {}",
                scene.name,
                result.ssim,
                result.threshold,
                result.max_delta,
                result.max_pixel_delta_threshold
            ));
        }
    }
    if compared == 0 {
        if let Some(scene) = options.scene.as_deref() {
            anyhow::bail!("visual scene '{scene}' not found");
        }
        anyhow::bail!("no visual scenes configured");
    }
    if failures.is_empty() {
        Ok(written_results)
    } else {
        anyhow::bail!("{}", failures.join("\n"));
    }
}

fn selected_scenes<'a>(
    visual: &'a VisualConfig,
    scene_name: Option<&str>,
) -> Vec<&'a crate::core::config::Scene> {
    match scene_name {
        Some(scene_name) => visual
            .scene
            .iter()
            .filter(|scene| scene.name == scene_name)
            .collect(),
        None => visual.scene.iter().collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn rgba_test_format() -> vnc::PixelFormat {
        vnc::PixelFormat {
            bits_per_pixel: 32,
            depth: 24,
            big_endian: false,
            true_colour: true,
            red_max: 255,
            green_max: 255,
            blue_max: 255,
            red_shift: 16,
            green_shift: 8,
            blue_shift: 0,
        }
    }

    fn encode_rect_pixels(rect: vnc::Rect, fill: impl Fn(usize, usize) -> [u8; 4]) -> Vec<u8> {
        let mut pixels = Vec::with_capacity(rect.width as usize * rect.height as usize * 4);
        for y in 0..rect.height as usize {
            for x in 0..rect.width as usize {
                let [r, g, b, _a] = fill(x, y);
                pixels.extend_from_slice(&[b, g, r, 0]);
            }
        }
        pixels
    }

    fn fill_expected_rgba(
        width: usize,
        height: usize,
        fill: impl Fn(usize, usize) -> [u8; 4],
    ) -> Vec<u8> {
        let mut rgba = vec![0; width * height * 4];
        for y in 0..height {
            for x in 0..width {
                let dst = (y * width + x) * 4;
                rgba[dst..dst + 4].copy_from_slice(&fill(x, y));
            }
        }
        rgba
    }

    fn apply_rects_in_order(
        width: u16,
        height: u16,
        rects: &[vnc::Rect],
        full_frame_fill: impl Fn(usize, usize) -> [u8; 4] + Copy,
    ) -> anyhow::Result<Vec<u8>> {
        let mut state = VncFramebufferState::new(width, height);
        let format = rgba_test_format();
        for rect in rects {
            let pixels = encode_rect_pixels(*rect, |x, y| {
                full_frame_fill(rect.left as usize + x, rect.top as usize + y)
            });
            state.blit_pixels(format, *rect, &pixels)?;
        }
        anyhow::ensure!(state.is_complete(), "expected full coverage");
        Ok(state.rgba)
    }

    #[test]
    fn channel_scales_to_u8() {
        assert_eq!(channel(0x00ff0000, 255, 16), 255);
        assert_eq!(channel(0x00008000, 255, 8), 128);
    }

    #[test]
    fn blank_image_fails_nonblank_validation() {
        let dir =
            std::env::temp_dir().join(format!("steampipe-blank-image-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let image = dir.join("blank.png");
        image::save_buffer(&image, &[0; 4 * 8 * 8], 8, 8, image::ColorType::Rgba8).unwrap();

        let err = validate_image_nonblank(&image).unwrap_err();
        assert!(err.to_string().contains("appears blank"));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn uniform_row_band_fails_nonblank_validation() {
        let dir =
            std::env::temp_dir().join(format!("steampipe-uniform-band-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let image = dir.join("band.png");
        let width = 64usize;
        let height = 64usize;
        let mut rgba = vec![0; width * height * 4];
        for y in NONBLANK_ROW_BAND_HEIGHT..height {
            for x in 0..width {
                let dst = (y * width + x) * 4;
                rgba[dst..dst + 4].copy_from_slice(&[x as u8, y as u8, 200, 255]);
            }
        }
        image::save_buffer(
            &image,
            &rgba,
            width as u32,
            height as u32,
            image::ColorType::Rgba8,
        )
        .unwrap();

        let err = validate_image_nonblank(&image).unwrap_err();
        let err = err.to_string();
        assert!(
            err.contains("entirely the first pixel colour") || err.contains("entirely zero-alpha")
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn partial_rect_failure_repro_tracks_incomplete_coverage() {
        let mut state = VncFramebufferState::new(4, 4);
        let rect = vnc::Rect {
            left: 0,
            top: 0,
            width: 2,
            height: 2,
        };
        let pixels = encode_rect_pixels(rect, |_x, _y| [255, 0, 0, 255]);

        state
            .blit_pixels(rgba_test_format(), rect, &pixels)
            .unwrap();
        state.end_of_frame_events += 1;

        assert!(!state.is_complete());
        assert_eq!(state.covered_pixels, 4);
        assert_eq!(state.end_of_frame_events, 1);
        assert_eq!(
            &state.rgba[..16],
            &[255, 0, 0, 255, 255, 0, 0, 255, 0, 0, 0, 0, 0, 0, 0, 0]
        );
    }

    #[test]
    fn partial_rect_then_complete_succeeds() {
        let mut state = VncFramebufferState::new(4, 4);
        let top = vnc::Rect {
            left: 0,
            top: 0,
            width: 4,
            height: 2,
        };
        let bottom = vnc::Rect {
            left: 0,
            top: 2,
            width: 4,
            height: 2,
        };

        let top_pixels = encode_rect_pixels(top, |_x, _y| [10, 20, 30, 255]);
        state
            .blit_pixels(rgba_test_format(), top, &top_pixels)
            .unwrap();
        state.end_of_frame_events += 1;
        assert!(!state.is_complete());

        let bottom_pixels = encode_rect_pixels(bottom, |_x, _y| [40, 50, 60, 255]);
        state
            .blit_pixels(rgba_test_format(), bottom, &bottom_pixels)
            .unwrap();
        state.end_of_frame_events += 1;

        assert!(state.is_complete());
        assert_eq!(state.put_pixels_events, 2);
        assert_eq!(state.end_of_frame_events, 2);
        assert_eq!(state.coverage_percent(), 100.0);
        assert_eq!(
            state.rgba,
            fill_expected_rgba(4, 4, |_x, y| {
                if y < 2 {
                    [10, 20, 30, 255]
                } else {
                    [40, 50, 60, 255]
                }
            })
        );
    }

    #[test]
    fn timeout_error_includes_coverage_percentage() {
        let mut state = VncFramebufferState::new(4, 4);
        let rect = vnc::Rect {
            left: 0,
            top: 0,
            width: 2,
            height: 2,
        };
        let pixels = encode_rect_pixels(rect, |_x, _y| [255, 255, 255, 255]);
        state
            .blit_pixels(rgba_test_format(), rect, &pixels)
            .unwrap();
        state.end_of_frame_events = 3;
        state.last_rfb_error = Some("operation timed out".to_string());

        let message = state.timeout_error(4);
        assert!(message.contains("coverage 25.0%"));
        assert!(message.contains("PutPixels events 1"));
        assert!(message.contains("EndOfFrame events 3"));
        assert!(message.contains("last RFB error operation timed out"));
        assert!(message.contains("update requests 4"));
    }

    #[test]
    fn full_coverage_rect_permutations_match_monolithic_baseline() {
        let width = 320u16;
        let height = 240u16;
        let fill = |x: usize, y: usize| -> [u8; 4] {
            [(x % 251) as u8, (y % 241) as u8, ((x + y) % 239) as u8, 255]
        };
        let rects = [
            vnc::Rect {
                left: 0,
                top: 0,
                width: 160,
                height: 120,
            },
            vnc::Rect {
                left: 160,
                top: 0,
                width: 160,
                height: 120,
            },
            vnc::Rect {
                left: 0,
                top: 120,
                width: 160,
                height: 120,
            },
            vnc::Rect {
                left: 160,
                top: 120,
                width: 160,
                height: 120,
            },
        ];
        let baseline_rect = [vnc::Rect {
            left: 0,
            top: 0,
            width,
            height,
        }];
        let baseline = apply_rects_in_order(width, height, &baseline_rect, fill).unwrap();
        let orders = [[0usize, 1, 2, 3], [3usize, 2, 1, 0], [1usize, 3, 0, 2]];

        for order in orders {
            let permuted = order.map(|index| rects[index]);
            let actual = apply_rects_in_order(width, height, &permuted, fill).unwrap();
            assert_eq!(actual, baseline);
        }
    }

    #[test]
    fn validator_receives_visual_env() {
        let dir =
            std::env::temp_dir().join(format!("steampipe-validator-env-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let image = dir.join("vm-1.png");
        image::save_buffer(&image, &[255; 4], 1, 1, image::ColorType::Rgba8).unwrap();
        let env_out = dir.join("env.txt");
        let validator = format!(
            "printf '%s|%s|%s|%s' \"$STEAMPIPE_VISUAL_IMAGE\" \"$STEAMPIPE_VISUAL_VM\" \"$STEAMPIPE_VISUAL_RUN\" \"$STEAMPIPE_VISUAL_BACKEND\" > {}",
            env_out.display()
        );
        let options = ScreenshotOptions {
            backend: ScreenshotBackend::Vnc,
            validator: Some(validator.clone()),
            golden: None,
            scene: None,
            run_label: Some("run-7".to_string()),
            window_target: None,
            allow_vnc_input: false,
        };

        run_validator(&validator, &image, "vm-1", &options).unwrap();

        let env = fs::read_to_string(&env_out).unwrap();
        assert_eq!(env, format!("{}|vm-1|run-7|vnc", image.display()));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn wayvnc_start_script_sets_runtime_display_and_diagnostics() {
        let script = wayvnc_start_script("chessbender", false);
        assert!(script.contains("XDG_RUNTIME_DIR=/tmp/runtime-chessbender"));
        assert!(script.contains("WAYLAND_DISPLAY=wayland-1"));
        assert!(script.contains("VNC capture requires a sway/sway-gpu session"));
        assert!(script.contains("swaymsg -r -t get_outputs"));
        assert!(script.contains("current_resizing_mode"));
        assert!(
            script.contains(
                "wayvnc --disable-input --disable-resizing --log-level=info 0.0.0.0 5900"
            )
        );
        assert!(script.contains("tail -n 40 \"$XDG_RUNTIME_DIR/wayvnc.log\""));
    }

    #[test]
    fn wayvnc_start_script_can_enable_input_for_scene_scripting() {
        let script = wayvnc_start_script("chessbender", true);
        assert!(!script.contains("wayvnc --disable-input --log-level=info"));
        assert!(script.contains("desired_input_mode=\"enabled\""));
    }
}
