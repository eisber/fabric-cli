// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Side-effect-free ownership selection; each host still validates its full schema.

use fabric_cli_mutations::v2_link::LinkOwner;
use std::ffi::OsString;

/// Identify the first positional root LINK owner, including malformed children.
///
/// Option values are never destinations. Help and invalid flags remain for the
/// selected host's parser, so neither dispatch order nor JSON input selects an owner.
pub fn root_owner(args: &[OsString]) -> Option<LinkOwner> {
    if args.first()?.to_str()? != "link" {
        return None;
    }
    let mut args = args.iter().skip(1);
    let mut positional = false;
    while let Some(arg) = args.next() {
        let arg = arg.to_str()?;
        if !positional {
            match arg {
                "--" => {
                    positional = true;
                    continue;
                }
                "--input" => {
                    args.next()?;
                    continue;
                }
                value if value.starts_with('-') => continue,
                _ => (),
            }
        }
        return match arg.split('/').nth(2)? {
            owner if owner.ends_with(".DataAgent") => Some(LinkOwner::DataAgent),
            owner if owner.ends_with(".KQLDatabase") => Some(LinkOwner::KqlDatabase),
            _ => None,
        };
    }
    None
}
