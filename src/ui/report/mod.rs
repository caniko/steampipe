use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use base64::Engine as _;

use crate::core::config;
use crate::core::config::ClusterConfig;
use crate::harness::history::{self, TestResult};

const STYLE: &str = r#"
:root { color-scheme: light; font-family: ui-sans-serif, system-ui, sans-serif; }
body { margin: 2rem; background: #f4f4ef; color: #151515; }
main { max-width: 1280px; margin: 0 auto; }
h1, h2, h3 { font-family: ui-monospace, SFMono-Regular, monospace; }
.summary, .vm-card, .scene-card, .table-card { background: #fffdfa; border: 1px solid #d6d0c7; border-radius: 10px; padding: 1rem; margin-bottom: 1rem; box-shadow: 0 10px 30px rgba(0,0,0,0.04); }
.grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(320px, 1fr)); gap: 1rem; }
.triptych { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: 0.75rem; }
.triptych figure, .timelapse-strip figure { margin: 0; }
img { max-width: 100%; height: auto; border: 1px solid #d6d0c7; background: #fbf8f2; }
code, table { font-family: ui-monospace, SFMono-Regular, monospace; }
.badge { display: inline-block; padding: 0.2rem 0.5rem; border-radius: 999px; font-size: 0.85rem; border: 1px solid #b9b2a8; }
.badge.pass { background: #e3f4e0; color: #215c1f; }
.badge.fail { background: #fbe0dc; color: #8e281e; }
.kv { display: grid; grid-template-columns: 12rem 1fr; gap: 0.4rem 1rem; }
.timelapse-strip { display: flex; gap: 0.75rem; overflow-x: auto; padding-bottom: 0.5rem; }
table { width: 100%; border-collapse: collapse; }
th, td { text-align: left; padding: 0.45rem 0.55rem; border-bottom: 1px solid #e7e0d6; vertical-align: top; }
details pre { white-space: pre-wrap; overflow-wrap: anywhere; }
@media (max-width: 800px) { .triptych, .kv { grid-template-columns: 1fr; } body { margin: 1rem; } }
"#;

#[derive(Clone)]
struct SceneView {
    scene: String,
    vm: String,
    passed: bool,
    ssim: String,
    max_delta: u8,
    actual_src: String,
    golden_src: String,
    diff_src: String,
}

#[derive(Clone)]
struct VmView {
    name: String,
    scenes: Vec<SceneView>,
    timelapse_frames: Vec<AssetView>,
    video: Option<AssetView>,
}

#[derive(Clone)]
struct AssetView {
    label: String,
    src: String,
}

#[derive(Clone)]
struct PreflightRow {
    vm: String,
    probe: String,
    detail: String,
}

#[derive(Clone)]
struct ReadinessRow {
    vm: String,
    state: String,
    detail: String,
}

pub fn generate<S>(
    config: &ClusterConfig<S>,
    project_root: &Path,
    run_label: Option<&str>,
    output: Option<&Path>,
    single_file: bool,
) -> anyhow::Result<PathBuf> {
    let history = history::load(&config.state_dir);
    let result = select_result(&history.results, run_label)?;
    let resolved_run_label = run_label
        .map(str::to_string)
        .or_else(|| result.visual_results.first().map(|result| result.run_label.clone()))
        .ok_or_else(|| anyhow::anyhow!("could not resolve run label for report"))?;

    let visual = load_visual_config(project_root)?;
    let run_root = visual
        .diff_dir
        .clone()
        .unwrap_or_else(|| project_root.join("tests/visual/diff"))
        .join(&resolved_run_label);
    let output_dir = output
        .map(Path::to_path_buf)
        .unwrap_or_else(|| run_root.join("report"));
    std::fs::create_dir_all(&output_dir)?;

    let mut grouped: BTreeMap<String, Vec<SceneView>> = BTreeMap::new();
    for item in &result.visual_results {
        let vm_name = if item.vm.is_empty() { "vm-1" } else { item.vm.as_str() };
        grouped.entry(vm_name.to_string()).or_default().push(SceneView {
            scene: item.scene.clone(),
            vm: vm_name.to_string(),
            passed: item.passed,
            ssim: format!("{:.5}", item.ssim),
            max_delta: item.max_delta,
            actual_src: asset_src(&output_dir, &item.actual_path, single_file)?,
            golden_src: asset_src(&output_dir, &item.golden_path, single_file)?,
            diff_src: asset_src(&output_dir, &item.diff_path, single_file)?,
        });
    }

    let timelapse_root = run_root.join("timelapse");
    let video_root = run_root.join("video");
    let mut vms = Vec::new();
    for (name, scenes) in grouped {
        let timelapse_frames = collect_png_assets(&output_dir, &timelapse_root.join(&name), single_file)?;
        let video_path = video_root.join(format!("{name}.webm"));
        let video = if video_path.exists() {
            Some(AssetView {
                label: name.clone(),
                src: asset_src(&output_dir, &video_path, single_file)?,
            })
        } else {
            None
        };
        vms.push(VmView {
            name,
            scenes,
            timelapse_frames,
            video,
        });
    }

    let preflight: Vec<PreflightRow> = result
        .gpu_preflight
        .iter()
        .map(|row| PreflightRow {
            vm: row.vm.clone(),
            probe: format!("{:?}", row.probe_result),
            detail: row
                .failure_kind
                .as_ref()
                .map(|kind| format!("{kind:?}"))
                .unwrap_or_else(|| "PASS".to_string()),
        })
        .collect();
    let readiness: Vec<ReadinessRow> = result
        .readiness
        .iter()
        .map(|row| ReadinessRow {
            vm: row.name.clone(),
            state: row
                .readiness
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_else(|| "ERROR".to_string()),
            detail: row.error.clone().unwrap_or_else(|| {
                row.readiness
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_else(|| "unknown".to_string())
            }),
        })
        .collect();

    let result_label = if result.failed == 0 { "PASS" } else { "FAIL" };
    let visual_summary = history::summarize_visual_results(&result.visual_results)
        .map(|summary| format!("pass={} fail={}", summary.passed, summary.failed))
        .unwrap_or_else(|| "no scene comparisons".to_string());
    let html = render_html(
        &resolved_run_label,
        result,
        result_label,
        &visual_summary,
        &preflight,
        &readiness,
        &vms,
    );

    let index_path = output_dir.join("index.html");
    std::fs::write(&index_path, html)?;
    Ok(index_path)
}

fn render_html(
    run_label: &str,
    result: &TestResult,
    result_label: &str,
    visual_summary: &str,
    preflight: &[PreflightRow],
    readiness: &[ReadinessRow],
    vms: &[VmView],
) -> String {
    let mut html = String::new();
    html.push_str("<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">");
    html.push_str(&format!("<title>Visual Report {}</title>", escape_html(run_label)));
    html.push_str("<style>");
    html.push_str(STYLE);
    html.push_str("</style></head><body><main>");
    html.push_str("<section class=\"summary\"><h1>");
    html.push_str(&escape_html(&format!("Visual Report {run_label}")));
    html.push_str("</h1><div class=\"kv\">");
    for (label, value) in [
        ("Timestamp", result.timestamp.as_str()),
        ("Network", result.network.as_str()),
        ("Run Result", result_label),
        ("Visual", visual_summary),
    ] {
        html.push_str(&format!(
            "<div>{}</div><div>{}</div>",
            escape_html(label),
            escape_html(value)
        ));
    }
    html.push_str(&format!(
        "<div>Players</div><div>{}</div><div>VMs</div><div>{}</div>",
        result.players, result.vm_count
    ));
    html.push_str("</div></section>");

    if !preflight.is_empty() {
        html.push_str("<section class=\"table-card\"><h2>GPU Preflight</h2><table><thead><tr><th>VM</th><th>Probe</th><th>Detail</th></tr></thead><tbody>");
        for row in preflight {
            html.push_str(&format!(
                "<tr><td>{}</td><td>{}</td><td>{}</td></tr>",
                escape_html(&row.vm),
                escape_html(&row.probe),
                escape_html(&row.detail)
            ));
        }
        html.push_str("</tbody></table></section>");
    }

    if !readiness.is_empty() {
        html.push_str("<section class=\"table-card\"><h2>Readiness</h2><table><thead><tr><th>VM</th><th>State</th><th>Detail</th></tr></thead><tbody>");
        for row in readiness {
            html.push_str(&format!(
                "<tr><td>{}</td><td>{}</td><td>{}</td></tr>",
                escape_html(&row.vm),
                escape_html(&row.state),
                escape_html(&row.detail)
            ));
        }
        html.push_str("</tbody></table></section>");
    }

    for vm in vms {
        html.push_str(&format!(
            "<section class=\"vm-card\"><h2>{}</h2>",
            escape_html(&vm.name)
        ));
        if !vm.scenes.is_empty() {
            html.push_str("<div class=\"grid\">");
            for scene in &vm.scenes {
                let badge = if scene.passed { "pass" } else { "fail" };
                let label = if scene.passed { "PASS" } else { "FAIL" };
                html.push_str("<article class=\"scene-card\">");
                html.push_str(&format!(
                    "<h3>{}</h3><p><span class=\"badge {}\">{}</span> SSIM {} | max delta {}</p>",
                    escape_html(&scene.scene),
                    badge,
                    label,
                    escape_html(&scene.ssim),
                    scene.max_delta
                ));
                html.push_str("<div class=\"triptych\">");
                for (caption, src, alt) in [
                    (
                        "Actual",
                        scene.actual_src.as_str(),
                        format!("Actual capture for {} on {}", scene.scene, scene.vm),
                    ),
                    (
                        "Golden",
                        scene.golden_src.as_str(),
                        format!("Golden reference for {}", scene.scene),
                    ),
                    (
                        "Diff",
                        scene.diff_src.as_str(),
                        format!("Diff image for {} on {}", scene.scene, scene.vm),
                    ),
                ] {
                    html.push_str(&format!(
                        "<figure><img src=\"{}\" alt=\"{}\"><figcaption>{}</figcaption></figure>",
                        escape_html(src),
                        escape_html(&alt),
                        caption
                    ));
                }
                html.push_str("</div></article>");
            }
            html.push_str("</div>");
        }
        if !vm.timelapse_frames.is_empty() {
            html.push_str("<section class=\"scene-card\"><h3>Timelapse</h3><div class=\"timelapse-strip\">");
            for frame in &vm.timelapse_frames {
                html.push_str(&format!(
                    "<figure><img src=\"{}\" alt=\"Timelapse frame {} for {}\"><figcaption>{}</figcaption></figure>",
                    escape_html(&frame.src),
                    escape_html(&frame.label),
                    escape_html(&vm.name),
                    escape_html(&frame.label)
                ));
            }
            html.push_str("</div></section>");
        }
        if let Some(video) = &vm.video {
            html.push_str(&format!(
                "<section class=\"scene-card\"><h3>Video</h3><video controls preload=\"metadata\" src=\"{}\">Video capture for {}</video></section>",
                escape_html(&video.src),
                escape_html(&vm.name)
            ));
        }
        html.push_str("</section>");
    }
    html.push_str("</main></body></html>");
    html
}

fn escape_html(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn select_result<'a>(results: &'a [TestResult], run_label: Option<&str>) -> anyhow::Result<&'a TestResult> {
    if let Some(run_label) = run_label {
        results
            .iter()
            .rev()
            .find(|result| result.visual_results.iter().any(|item| item.run_label == run_label))
            .ok_or_else(|| anyhow::anyhow!("no test history entry found for run label '{run_label}'"))
    } else {
        results
            .iter()
            .rev()
            .find(|result| !result.visual_results.is_empty())
            .ok_or_else(|| anyhow::anyhow!("no history entry with visual results found"))
    }
}

fn load_visual_config(project_root: &Path) -> anyhow::Result<config::VisualConfig> {
    let mut project_config = config::load_project_config(project_root)
        .ok_or_else(|| anyhow::anyhow!("visual report requires steampipe.toml"))?;
    let visual = project_config
        .visual
        .as_mut()
        .ok_or_else(|| anyhow::anyhow!("visual report requires a [visual] block"))?;
    visual.resolve_paths(project_root);
    Ok(visual.clone())
}

fn collect_png_assets(output_dir: &Path, dir: &Path, single_file: bool) -> anyhow::Result<Vec<AssetView>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("png"))
        .collect();
    entries.sort();
    entries
        .into_iter()
        .map(|path| {
            let label = path.file_name().and_then(|name| name.to_str()).unwrap_or("frame").to_string();
            Ok(AssetView {
                label,
                src: asset_src(output_dir, &path, single_file)?,
            })
        })
        .collect()
}

fn asset_src(output_dir: &Path, path: &Path, single_file: bool) -> anyhow::Result<String> {
    if single_file {
        let bytes = std::fs::read(path).with_context(|| format!("read asset {}", path.display()))?;
        let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
        let mime = match path.extension().and_then(|ext| ext.to_str()) {
            Some("png") => "image/png",
            Some("webm") => "video/webm",
            _ => "application/octet-stream",
        };
        Ok(format!("data:{mime};base64,{encoded}"))
    } else {
        Ok(relative_path(output_dir, path).display().to_string())
    }
}

fn relative_path(from_dir: &Path, to_path: &Path) -> PathBuf {
    let from_components: Vec<_> = from_dir.components().collect();
    let to_components: Vec<_> = to_path.components().collect();
    let common_len = from_components
        .iter()
        .zip(&to_components)
        .take_while(|(a, b)| a == b)
        .count();

    let mut relative = PathBuf::new();
    for _ in common_len..from_components.len() {
        relative.push("..");
    }
    for component in &to_components[common_len..] {
        relative.push(component.as_os_str());
    }
    relative
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_path_walks_to_sibling_file() {
        let from = Path::new("/tmp/a/report");
        let to = Path::new("/tmp/a/run/vm-1.png");
        assert_eq!(relative_path(from, to), PathBuf::from("../run/vm-1.png"));
    }
}
