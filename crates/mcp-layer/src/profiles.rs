//! Declarative job profiles and runner registration.

use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use toml::Table;

use crate::jobs::JobSpec;

/// Implementations must deserialize into a typed, deny-unknown-fields parameter struct.
pub trait Runner: Send + Sync {
    fn name(&self) -> &str;
    fn build(&self, params: &Table) -> Result<JobSpec>;
}

#[derive(Clone, Default)]
pub struct RunnerRegistry(BTreeMap<String, Arc<dyn Runner>>);

impl RunnerRegistry {
    pub fn register(&mut self, runner: impl Runner + 'static) -> Result<()> {
        let name = runner.name().to_owned();
        ensure!(!self.0.contains_key(&name), "duplicate runner: {name}");
        self.0.insert(name, Arc::new(runner));
        Ok(())
    }

    pub fn get(&self, name: &str) -> Option<&dyn Runner> {
        self.0.get(name).map(Arc::as_ref)
    }
}

fn one() -> u32 { 1 }
fn yes() -> bool { true }

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Cell {
    pub runner: String,
    #[serde(default)]
    pub params: Table,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub runner: Option<String>,
    #[serde(default)]
    pub params: Table,
    #[serde(default)]
    pub cells: Vec<Cell>,
    #[serde(default = "one")]
    pub repeats: u32,
    #[serde(default = "yes")]
    pub stop_on_failure: bool,
    pub timeout_secs: Option<u64>,
    #[serde(default)]
    pub locks: Vec<String>,
}

impl Profile {
    fn validate(&self) -> Result<()> {
        ensure!(self.runner.is_some() != !self.cells.is_empty(),
            "exactly one of runner or cells is required");
        ensure!(self.repeats > 0, "repeats must be positive");
        Ok(())
    }
}

#[derive(Default)]
pub struct Profiles(BTreeMap<String, Profile>);

pub enum ProfileOrInline<'a> {
    Name(&'a str),
    Inline(&'a Profile),
}

impl Profiles {
    pub fn load(paths: &[PathBuf]) -> Result<Self> {
        let mut profiles = BTreeMap::new();
        for path in paths {
            let text = match std::fs::read_to_string(path) {
                Ok(text) => text,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
            };
            let root: Table = toml::from_str(&text)
                .with_context(|| format!("parsing {}", path.display()))?;
            if let Some(value) = root.get("job_profiles") {
                let entries = value.as_table().context("job_profiles must be a table")
                    .with_context(|| format!("parsing {}", path.display()))?;
                for (name, value) in entries {
                    let profile: Profile = value.clone().try_into()
                        .with_context(|| format!("profile {name} in {}", path.display()))?;
                    profile.validate()
                        .with_context(|| format!("profile {name} in {}", path.display()))?;
                    profiles.insert(name.clone(), profile);
                }
            }
        }
        Ok(Self(profiles))
    }

    pub fn list(&self) -> &BTreeMap<String, Profile> { &self.0 }

    pub fn resolve(&self, input: ProfileOrInline<'_>, overrides: Table,
        registry: &RunnerRegistry) -> Result<Vec<JobSpec>>
    {
        let profile = match input {
            ProfileOrInline::Name(name) => self.0.get(name)
                .with_context(|| format!("unknown profile: {name}"))?,
            ProfileOrInline::Inline(profile) => profile,
        };
        profile.validate()?;
        ensure!(profile.runner.is_some() || overrides.is_empty(),
            "overrides are only supported for single-runner profiles");
        let cells = if let Some(runner) = &profile.runner {
            let mut params = profile.params.clone();
            params.extend(overrides);
            vec![Cell { runner: runner.clone(), params }]
        } else { profile.cells.clone() };
        let mut specs = Vec::new();
        for _ in 0..profile.repeats {
            for cell in &cells {
                let runner = registry.get(&cell.runner)
                    .with_context(|| format!("unknown runner: {}", cell.runner))?;
                let mut spec = runner.build(&cell.params)?;
                spec.runner.clone_from(&cell.runner);
                spec.sequence_timeout_secs = profile.timeout_secs;
                spec.locks.extend(profile.locks.iter().cloned());
                spec.locks.sort();
                spec.locks.dedup();
                specs.push(spec);
            }
        }
        Ok(specs)
    }
}
