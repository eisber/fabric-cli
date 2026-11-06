// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Synthetic deletion guard only: no Fabric variable API, I/O or remote effects.

#![deny(unnameable_test_items)]

use fabric_cli_context::NameHint;
use fabric_name_search::{NameSuggestion, SearchError, SearchOptions, suggest_names};

struct Variable<'a> {
    name: &'a str,
    value: &'a str,
    note: &'a str,
}

enum Attempt<'a> {
    Exact,
    NotFound(Vec<NameSuggestion<'a>>),
    HintsUnavailable(SearchError),
}

fn attempt_delete<'a>(
    query: &str,
    snapshot: &'a [Variable<'a>],
    effects: &mut Vec<&'a str>,
) -> Attempt<'a> {
    if let Some(exact) = snapshot.iter().find(|variable| variable.name == query) {
        // The only simulated effect is guarded by a byte-exact name match.
        effects.push(exact.name);
        return Attempt::Exact;
    }
    match suggest_names(
        query,
        snapshot.iter().map(|variable| variable.name),
        SearchOptions::default(),
    ) {
        Ok(names) => Attempt::NotFound(names),
        Err(error) => Attempt::HintsUnavailable(error),
    }
}

fn main() {
    let snapshot = [
        Variable {
            name: "environment",
            value: "synthetic-value-never-searched",
            note: "synthetic-note-never-searched",
        },
        Variable {
            name: "environment_name",
            value: "another-synthetic-value",
            note: "another-synthetic-note",
        },
    ];
    let mut effects = Vec::new();
    let Attempt::NotFound(suggestions) = attempt_delete("environmnt", &snapshot, &mut effects)
    else {
        panic!("a typo must remain NotFound");
    };
    assert!(effects.is_empty());
    assert_eq!(
        suggestions.first().map(|name| name.name),
        Some("environment")
    );
    println!("NotFound; zero simulated delete effects. Similar names (not selected):");
    for suggestion in suggestions {
        println!(
            "{}",
            NameHint {
                name: suggestion.name.into(),
                score: suggestion.score,
            }
        );
    }
    assert!(matches!(
        attempt_delete("environment", &snapshot, &mut effects),
        Attempt::Exact
    ));
    assert_eq!(effects, ["environment"]);
    assert_eq!(snapshot[0].value, "synthetic-value-never-searched");
    assert_eq!(snapshot[0].note, "synthetic-note-never-searched");

    // A lower-level engine rejection is still a failed attempt, never success.
    let long_query = "x".repeat(513);
    let Attempt::HintsUnavailable(error) = attempt_delete(&long_query, &snapshot, &mut effects)
    else {
        panic!("overlong names must retain an explicit hint failure");
    };
    let _ = error;
    assert_eq!(effects, ["environment"]);
}

#[cfg(test)]
mod tests {
    #[test]
    fn typo_has_zero_effects_and_exact_name_executes_once() {
        super::main();
    }

    #[test]
    fn notes_and_values_are_never_candidates_or_search_inputs() {
        let large_value = "x".repeat(1_048_577);
        let snapshot = [super::Variable {
            name: "environment",
            value: &large_value,
            note: "environmnt",
        }];
        let mut effects = Vec::new();
        let super::Attempt::NotFound(suggestions) =
            super::attempt_delete("environmnt", &snapshot, &mut effects)
        else {
            panic!("only the small name, not the over-limit value, must be searched");
        };
        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].name, "environment");
        assert!(effects.is_empty());
        assert_eq!(snapshot[0].value.len(), 1_048_577);
        assert_eq!(snapshot[0].note, "environmnt");
    }

    #[test]
    fn case_only_similarity_is_not_an_exact_delete() {
        let snapshot = [super::Variable {
            name: "environment",
            value: "synthetic",
            note: "synthetic",
        }];
        let mut effects = Vec::new();
        let super::Attempt::NotFound(suggestions) =
            super::attempt_delete("Environment", &snapshot, &mut effects)
        else {
            panic!("case differences must not execute a deletion");
        };
        assert!(effects.is_empty());
        assert_eq!(suggestions[0].name, "environment");
    }
}
