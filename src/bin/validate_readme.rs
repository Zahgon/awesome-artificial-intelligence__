//! CLI entry point — port of `scripts/validate_readme.py`'s `main()`.
//!
//! Usage: `validate_readme [readme] [--check-links] [--base <revision>]`.

use std::path::PathBuf;
use std::process::{Command, ExitCode};

use validate_readme::{check_links, validate_churn, validate_text};

fn main() -> ExitCode {
    let mut readme: Option<PathBuf> = None;
    let mut check_links_flag = false;
    let mut base: Option<String> = None;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--check-links" => check_links_flag = true,
            "--base" => {
                base = Some(match args.next() {
                    Some(v) => v,
                    None => {
                        eprintln!("error: --base requires a value");
                        return ExitCode::from(2);
                    }
                });
            }
            other if other.starts_with("--base=") => {
                base = Some(other["--base=".len()..].to_string());
            }
            other => {
                if readme.is_none() {
                    readme = Some(PathBuf::from(other));
                } else {
                    eprintln!("error: unexpected argument '{other}'");
                    return ExitCode::from(2);
                }
            }
        }
    }

    // `readme` positional defaults to README.md.
    let readme = readme.unwrap_or_else(|| PathBuf::from("README.md"));

    let current = match std::fs::read_to_string(&readme) {
        Ok(text) => text,
        Err(error) => {
            eprintln!("error: cannot read {}: {error}", readme.display());
            return ExitCode::from(2);
        }
    };

    let result = validate_text(&current);
    let mut errors = result.errors;
    let mut warnings = result.warnings;

    if check_links_flag {
        let (link_errors, link_warnings) = check_links(&result.resources);
        errors.extend(link_errors);
        warnings.extend(link_warnings);
    }

    if let Some(base) = base {
        // git show {base}:{readme.as_posix()}
        let target = format!("{base}:{}", readme.to_string_lossy().replace('\\', "/"));
        let output = Command::new("git").arg("show").arg(&target).output();
        match output {
            Ok(output) if output.status.success() => {
                let base_text = String::from_utf8_lossy(&output.stdout).to_string();
                errors.extend(validate_churn(&base_text, &current));
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                eprintln!("error: git show {target} failed: {stderr}");
                return ExitCode::from(2);
            }
            Err(error) => {
                eprintln!("error: failed to run git: {error}");
                return ExitCode::from(2);
            }
        }
    }

    for warning in &warnings {
        eprintln!("WARNING: {warning}");
    }
    for error in &errors {
        eprintln!("ERROR: {error}");
    }
    println!(
        "Validated {} resources with {} errors and {} warnings.",
        result.resources.len(),
        errors.len(),
        warnings.len()
    );

    if errors.is_empty() {
        ExitCode::from(0)
    } else {
        ExitCode::from(1)
    }
}
