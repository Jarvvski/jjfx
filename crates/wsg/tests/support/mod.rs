use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

use anyhow::{Result, bail};

#[derive(Debug, Clone)]
pub(crate) struct BinarySpec {
    label: &'static str,
    executable: PathBuf,
}

impl BinarySpec {
    pub(crate) fn new(label: &'static str, executable: PathBuf) -> Self {
        Self { label, executable }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.executable
    }

    pub(crate) fn run(&self, directory: &Path, args: &[&str]) -> CommandOutcome {
        let output = Command::new(&self.executable)
            .args(args)
            .current_dir(directory)
            .output()
            .unwrap_or_else(|error| {
                panic!(
                    "{} binary {} should run: {error}",
                    self.label,
                    self.executable.display()
                )
            });
        CommandOutcome {
            status: output.status,
            stdout: output.stdout,
            stderr: output.stderr,
        }
    }
}

#[derive(Debug)]
pub(crate) struct CommandOutcome {
    pub(crate) status: ExitStatus,
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
}

#[derive(Debug)]
pub(crate) struct ConformanceBinaries {
    pub(crate) rust: BinarySpec,
    pub(crate) go: BinarySpec,
    pub(crate) go_test: BinarySpec,
}

impl ConformanceBinaries {
    pub(crate) fn from_environment() -> Result<Self> {
        Self::from_explicit(
            PathBuf::from(env!("CARGO_BIN_EXE_wsg")),
            std::env::var_os("WSG_GO_BINARY").map(PathBuf::from),
            std::env::var_os("WSG_GO_TEST_BINARY").map(PathBuf::from),
        )
    }

    pub(crate) fn from_explicit(
        rust: PathBuf,
        go: Option<PathBuf>,
        go_test: Option<PathBuf>,
    ) -> Result<Self> {
        let mut missing = Vec::new();
        validate_path(&rust, "CARGO_BIN_EXE_wsg", &mut missing);

        let go = go.map(|path| {
            validate_path(&path, "WSG_GO_BINARY", &mut missing);
            BinarySpec::new("Go wsg", path)
        });
        let go_test = go_test.map(|path| {
            validate_path(&path, "WSG_GO_TEST_BINARY", &mut missing);
            BinarySpec::new("Go wsg test helper", path)
        });

        if go.is_none() {
            missing.push("WSG_GO_BINARY".to_owned());
        }
        if go_test.is_none() {
            missing.push("WSG_GO_TEST_BINARY".to_owned());
        }
        if !missing.is_empty() {
            bail!(
                "Go/Rust conformance requires explicit executable paths: {}",
                missing.join(", ")
            );
        }

        Ok(Self {
            rust: BinarySpec::new("Rust wsg", rust),
            go: go.expect("Go path was checked above"),
            go_test: go_test.expect("Go test path was checked above"),
        })
    }
}

fn validate_path(path: &Path, variable: &str, missing: &mut Vec<String>) {
    if !path.is_file() {
        missing.push(format!("{variable} ({})", path.display()));
    }
}
