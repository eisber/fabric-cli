// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use clap::{Arg, ArgAction, ArgMatches, Command};

/// Explicit update operation. Parsing never performs network or filesystem I/O.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UpdateAction {
    /// Discover and verify signed metadata only; do not download executable bytes.
    Check,
    /// Verify and queue a trusted bounded helper, or reconcile its prior result.
    Apply,
}

/// Non-secret update arguments for the optional, pinned-publisher host.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UpdateCommand {
    /// Checking and applying are deliberately distinct operations.
    pub action: UpdateAction,
    /// Optional exact `rust-vSEMVER`; otherwise inspect one bounded releases page.
    pub tag: Option<String>,
    /// Explicit cross-major installation opt-in; valid only with `--apply`.
    pub allow_major: bool,
}

pub(super) fn register(command: Command) -> Command {
    command.subcommand(
        Command::new("update")
            .about("Check or apply a pinned, signed native Fabric CLI release")
            .arg(
                Arg::new("check")
                    .long("check")
                    .action(ArgAction::SetTrue)
                    .conflicts_with("apply")
                    .help("Verify release metadata only (the default)"),
            )
            .arg(
                Arg::new("apply")
                    .long("apply")
                    .action(ArgAction::SetTrue)
                    .help("Queue or reconcile a trusted standalone update; pending work requires exit"),
            )
            .arg(
                Arg::new("allow-major")
                    .long("allow-major")
                    .action(ArgAction::SetTrue)
                    .requires("apply")
                    .conflicts_with("check")
                    .help("Explicitly permit a major upgrade; never enabled by automatic policy"),
            )
            .arg(
                Arg::new("tag")
                    .long("tag")
                    .value_name("RUST_RELEASE_TAG")
                    .help("Select exactly rust-vSEMVER; never a Python release tag"),
            ),
    )
}

pub(super) fn parse(matches: &ArgMatches) -> Result<UpdateCommand, clap::Error> {
    if matches.get_one::<String>("output_format").is_some() {
        return Err(super::invalid_arguments());
    }
    let update = matches
        .subcommand_matches("update")
        .expect("matched update schema");
    // Boolean flags have default match values; require the true apply action.
    if update.get_flag("allow-major") && !update.get_flag("apply") {
        return Err(super::invalid_arguments());
    }
    let tag = update.get_one::<String>("tag").cloned();
    if let Some(tag) = &tag {
        fabric_cli_update::ReleaseSelector::exact_tag(tag)
            .map_err(|_| super::invalid_arguments())?;
    }
    Ok(UpdateCommand {
        action: if update.get_flag("apply") {
            UpdateAction::Apply
        } else {
            UpdateAction::Check
        },
        tag,
        allow_major: update.get_flag("allow-major"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema() -> Command {
        register(
            Command::new("fab-rs")
                .arg(Arg::new("output_format").long("output_format").global(true)),
        )
    }

    #[test]
    fn check_apply_and_exact_tag_are_separate_and_non_secret() {
        for (arguments, action) in [
            (vec!["fab-rs", "update"], UpdateAction::Check),
            (vec!["fab-rs", "update", "--check"], UpdateAction::Check),
            (vec!["fab-rs", "update", "--apply"], UpdateAction::Apply),
        ] {
            let matches = schema().try_get_matches_from(arguments).unwrap();
            assert_eq!(parse(&matches).unwrap().action, action);
            assert!(!parse(&matches).unwrap().allow_major);
        }
        let matches = schema()
            .try_get_matches_from(["fab-rs", "update", "--check", "--tag", "rust-v1.2.3"])
            .unwrap();
        assert_eq!(parse(&matches).unwrap().tag.as_deref(), Some("rust-v1.2.3"));
    }

    #[test]
    fn conflicting_modes_python_tags_urls_and_output_override_are_rejected() {
        assert!(
            schema()
                .try_get_matches_from(["fab-rs", "update", "--check", "--apply"])
                .is_err()
        );
        for tag in [
            "v1.2.3",
            "1.2.3",
            "https://github.com/owner/repo",
            "rust-vnot-semver",
        ] {
            let matches = schema()
                .try_get_matches_from(["fab-rs", "update", "--tag", tag])
                .unwrap();
            assert!(parse(&matches).is_err());
        }
        let matches = schema()
            .try_get_matches_from(["fab-rs", "update", "--output_format", "text"])
            .unwrap();
        assert!(parse(&matches).is_err());
    }

    #[test]
    fn major_opt_in_is_explicit_and_only_valid_with_apply() {
        for arguments in [
            vec!["fab-rs", "update", "--allow-major"],
            vec!["fab-rs", "update", "--check", "--allow-major"],
        ] {
            if let Ok(matches) = schema().try_get_matches_from(arguments) {
                assert!(parse(&matches).is_err());
            }
        }
        let matches = schema()
            .try_get_matches_from([
                "fab-rs",
                "update",
                "--apply",
                "--allow-major",
                "--tag",
                "rust-v2.0.0",
            ])
            .unwrap();
        let command = parse(&matches).unwrap();
        assert_eq!(command.action, UpdateAction::Apply);
        assert!(command.allow_major);
        assert_eq!(command.tag.as_deref(), Some("rust-v2.0.0"));
    }
}
