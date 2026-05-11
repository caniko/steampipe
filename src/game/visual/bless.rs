use std::io::{IsTerminal as _, Write as _};
use std::path::{Path, PathBuf};

use crate::core::config::{Scene, VisualConfig};
use crate::game::visual::engine::{Engine, VisualError, VisualRunResult, write_result_artifacts};

pub fn record_scene(
    visual: &VisualConfig,
    scene: &Scene,
    actual_path: &Path,
    force: bool,
) -> anyhow::Result<PathBuf> {
    let golden_path = visual.golden_path(&scene.name)?;
    if golden_path.exists() && !force {
        anyhow::bail!(
            "{} already exists; use --force to overwrite",
            golden_path.display()
        );
    }
    if let Some(parent) = golden_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(actual_path, &golden_path)?;
    Ok(golden_path)
}

pub fn bless_scene(
    visual: &VisualConfig,
    scene: &Scene,
    run_label: &str,
    yes: bool,
) -> anyhow::Result<PathBuf> {
    let paths = visual.diff_path(&scene.name, run_label)?;
    if !paths.actual.exists() {
        anyhow::bail!(
            "cannot bless {}; missing actual capture {}",
            scene.name,
            paths.actual.display()
        );
    }
    let golden_path = visual.golden_path(&scene.name)?;
    if !yes && !confirm_bless(&scene.name, &paths.actual, &golden_path)? {
        anyhow::bail!("bless cancelled");
    }
    if let Some(parent) = golden_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(&paths.actual, &golden_path)?;
    Ok(golden_path)
}

pub fn diff_scene(
    visual: &VisualConfig,
    engine: &Engine,
    scene: &Scene,
    run_label: &str,
) -> Result<VisualRunResult, VisualError> {
    let paths = visual
        .diff_path(&scene.name, run_label)
        .map_err(|source| VisualError::Config {
            message: source.to_string(),
        })?;
    let result = engine.compare(&paths.actual, scene)?;
    write_result_artifacts(visual, scene, run_label, &paths.actual, &result)
}

fn confirm_bless(scene_name: &str, actual: &Path, golden: &Path) -> anyhow::Result<bool> {
    if !std::io::stdin().is_terminal() {
        anyhow::bail!("bless requires --yes when stdin is not interactive");
    }
    print!(
        "Overwrite golden for scene '{scene_name}' with {} -> {}? [y/N] ",
        actual.display(),
        golden.display()
    );
    std::io::stdout().flush()?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes" | "YES"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgba, RgbaImage};

    fn scene(name: &str) -> Scene {
        Scene {
            name: name.into(),
            ..Default::default()
        }
    }

    fn visual(tmp: &Path, scene: Scene) -> VisualConfig {
        VisualConfig {
            golden_dir: Some(tmp.join("golden")),
            diff_dir: Some(tmp.join("diff")),
            scene: vec![scene],
            ..Default::default()
        }
    }

    fn write_png(path: &Path, color: [u8; 4]) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        RgbaImage::from_pixel(64, 64, Rgba(color))
            .save(path)
            .unwrap();
    }

    #[test]
    fn record_writes_new_golden() {
        let tmp = tempfile::tempdir().unwrap();
        let scene = scene("main");
        let visual = visual(tmp.path(), scene.clone());
        let actual = tmp.path().join("actual.png");
        write_png(&actual, [1, 2, 3, 255]);
        let golden = record_scene(&visual, &scene, &actual, false).unwrap();
        assert!(golden.exists());
    }

    #[test]
    fn record_refuses_overwrite_without_force() {
        let tmp = tempfile::tempdir().unwrap();
        let scene = scene("main");
        let visual = visual(tmp.path(), scene.clone());
        let actual = tmp.path().join("actual.png");
        write_png(&actual, [1, 2, 3, 255]);
        record_scene(&visual, &scene, &actual, false).unwrap();
        let err = record_scene(&visual, &scene, &actual, false).unwrap_err();
        assert!(err.to_string().contains("--force"));
    }

    #[test]
    fn record_force_overwrites() {
        let tmp = tempfile::tempdir().unwrap();
        let scene = scene("main");
        let visual = visual(tmp.path(), scene.clone());
        let actual = tmp.path().join("actual.png");
        write_png(&actual, [1, 2, 3, 255]);
        record_scene(&visual, &scene, &actual, false).unwrap();
        write_png(&actual, [9, 8, 7, 255]);
        record_scene(&visual, &scene, &actual, true).unwrap();
        assert!(visual.golden_path("main").unwrap().exists());
    }

    #[test]
    fn bless_requires_existing_actual() {
        let tmp = tempfile::tempdir().unwrap();
        let scene = scene("main");
        let visual = visual(tmp.path(), scene.clone());
        let err = bless_scene(&visual, &scene, "run-1", true).unwrap_err();
        assert!(err.to_string().contains("missing actual"));
    }

    #[test]
    fn diff_rewrites_sidecar_from_existing_actual() {
        let tmp = tempfile::tempdir().unwrap();
        let scene = scene("main");
        let visual = visual(tmp.path(), scene.clone());
        write_png(&visual.golden_path("main").unwrap(), [1, 2, 3, 255]);
        let paths = visual.diff_path("main", "run-1").unwrap();
        write_png(&paths.actual, [1, 2, 3, 255]);
        let engine = Engine::new(visual.clone());
        let result = diff_scene(&visual, &engine, &scene, "run-1").unwrap();
        assert!(result.passed);
        assert!(result.result_path.exists());
    }
}
