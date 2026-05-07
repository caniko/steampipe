use std::io::Write as _;
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use crate::core::backend::Backend;
use crate::core::config::{ClusterConfig, VmDef, par_each_vm};
use crate::ui::cli::ScreenshotBackend;

#[derive(Clone, Debug)]
pub struct ScreenshotOptions {
    pub backend: ScreenshotBackend,
    pub validator: Option<String>,
    pub run_label: Option<String>,
}

impl Default for ScreenshotOptions {
    fn default() -> Self {
        Self {
            backend: ScreenshotBackend::Grim,
            validator: None,
            run_label: None,
        }
    }
}

/// Capture a screenshot from an instance using grim (Wayland screenshot tool).
pub async fn screenshot_wayland_vm(
    backend: &Backend,
    vm: &VmDef,
    output_dir: &Path,
) -> anyhow::Result<PathBuf> {
    let remote_path = "/tmp/screenshot.png";
    let cmd = format!(
        "export XDG_RUNTIME_DIR=/tmp/runtime-$(whoami); \
         export WAYLAND_DISPLAY=wayland-1; \
         grim {remote_path} 2>/dev/null && echo OK || echo FAIL"
    );
    let result = backend.run_cmd(&vm.ip, &cmd).await;
    if !result.stdout.trim().contains("OK") {
        anyhow::bail!("{}: screenshot failed (is weston/sway running?)", vm.name);
    }

    // Download via upload (rsync reverse direction)
    let local_path = output_dir.join(format!("{}.png", vm.name));
    let sources = [Path::new(remote_path)];
    backend
        .upload(&sources, &vm.ip, local_path.to_str().unwrap_or("."))
        .await?;

    Ok(local_path)
}

/// Start wayvnc inside a VM. This requires a wlroots compositor such as sway.
pub async fn ensure_wayvnc(backend: &Backend, vm: &VmDef, vm_user: &str) -> anyhow::Result<()> {
    let cmd = format!(
        r#"
export XDG_RUNTIME_DIR=/tmp/runtime-{vm_user}
export WAYLAND_DISPLAY=wayland-1
if ! pgrep -x wayvnc >/dev/null; then
    nohup wayvnc 0.0.0.0 5900 >$XDG_RUNTIME_DIR/wayvnc.log 2>&1 &
    sleep 1
fi
pgrep -x wayvnc >/dev/null && echo OK || echo FAIL
"#
    );
    let result = backend.run_cmd(&vm.ip, &cmd).await;
    if result.stdout.trim().contains("OK") {
        Ok(())
    } else {
        anyhow::bail!(
            "{}: wayvnc failed to start; VNC screenshots require a sway/sway-gpu display",
            vm.name
        );
    }
}

pub async fn ensure_wayvnc_all<S>(config: &ClusterConfig<S>) -> anyhow::Result<()> {
    let backend = config.backend.clone();
    let vm_user = config.vm_user.clone();
    let results = par_each_vm(&config.vms, |vm| {
        let backend = backend.clone();
        let vm_user = vm_user.clone();
        async move {
            let name = vm.name.to_string();
            (name, ensure_wayvnc(&backend, &vm, &vm_user).await)
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

pub async fn screenshot_vnc_vm(vm: &VmDef, output_dir: &Path) -> anyhow::Result<PathBuf> {
    let local_path = output_dir.join(format!("{}.png", vm.name));
    let ip = vm.ip.to_string();
    let path = local_path.clone();
    tokio::task::spawn_blocking(move || capture_vnc_framebuffer(&ip, &path))
        .await
        .map_err(|e| anyhow::anyhow!("{}: VNC screenshot task failed: {e}", vm.name))??;
    Ok(local_path)
}

fn capture_vnc_framebuffer(ip: &str, path: &Path) -> anyhow::Result<()> {
    let stream = TcpStream::connect((ip, 5900))?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;

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
    client
        .set_encodings(&[vnc::Encoding::Raw])
        .map_err(|e| anyhow::anyhow!("set VNC encodings: {e}"))?;

    let rect = vnc::Rect {
        left: 0,
        top: 0,
        width,
        height,
    };
    client
        .request_update(rect, false)
        .map_err(|e| anyhow::anyhow!("request VNC framebuffer update: {e}"))?;

    let mut rgba = vec![0; width as usize * height as usize * 4];
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut saw_pixels = false;
    while Instant::now() < deadline {
        for event in client.poll_iter() {
            match event {
                vnc::client::Event::PutPixels(rect, pixels) => {
                    blit_vnc_pixels(&mut rgba, width, desired_format, rect, &pixels)?;
                    saw_pixels = true;
                }
                vnc::client::Event::EndOfFrame if saw_pixels => {
                    image::save_buffer(
                        path,
                        &rgba,
                        width as u32,
                        height as u32,
                        image::ColorType::Rgba8,
                    )?;
                    validate_image_nonblank(path)?;
                    return Ok(());
                }
                vnc::client::Event::Disconnected(error) => {
                    anyhow::bail!("VNC disconnected: {error:?}");
                }
                _ => {}
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    anyhow::bail!("VNC framebuffer update timed out");
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
        anyhow::bail!("short VNC pixel buffer: got {}, expected {expected}", pixels.len());
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
            if distinct > 32 && max_luma.saturating_sub(min_luma) > 8 {
                return Ok(());
            }
        }
    }

    anyhow::bail!(
        "VNC screenshot {} appears blank (luma range {}..{}, distinct samples {})",
        path.display(),
        min_luma,
        max_luma,
        distinct
    );
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
        ensure_wayvnc_all(config).await?;
    }

    let results = par_each_vm(&config.vms, |vm| {
        let backend = backend.clone();
        let output_dir = output_dir.to_owned();
        let options = options.clone();
        async move {
            let result = match options.backend {
                ScreenshotBackend::Grim => screenshot_wayland_vm(&backend, &vm, &output_dir).await,
                ScreenshotBackend::Vnc => screenshot_vnc_vm(&vm, &output_dir).await,
            };
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
                if let Some(validator) = options.validator.as_deref() {
                    if let Err(error) = run_validator(validator, &path, name.as_ref(), options) {
                        failures.push(format!("{name}: {error}"));
                    }
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
) {
    let screenshot_dir = output_dir.join(format!("screenshots/run-{run_num}"));
    let options = ScreenshotOptions {
        run_label: Some(format!("run-{run_num}")),
        ..options.clone()
    };
    if let Err(e) = screenshot_all(config, &screenshot_dir, &options).await {
        eprintln!("  Screenshot capture failed: {e}");
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_scales_to_u8() {
        assert_eq!(channel(0x00ff0000, 255, 16), 255);
        assert_eq!(channel(0x00008000, 255, 8), 128);
    }

    #[test]
    fn blank_image_fails_nonblank_validation() {
        let dir = std::env::temp_dir().join(format!(
            "steampipe-blank-image-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let image = dir.join("blank.png");
        image::save_buffer(&image, &[0; 4 * 8 * 8], 8, 8, image::ColorType::Rgba8).unwrap();

        let err = validate_image_nonblank(&image).unwrap_err();
        assert!(err.to_string().contains("appears blank"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn validator_receives_visual_env() {
        let dir = std::env::temp_dir().join(format!(
            "steampipe-validator-env-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
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
            run_label: Some("run-7".to_string()),
        };

        run_validator(&validator, &image, "vm-1", &options).unwrap();

        let env = std::fs::read_to_string(&env_out).unwrap();
        assert_eq!(
            env,
            format!("{}|vm-1|run-7|vnc", image.display())
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
