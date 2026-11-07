// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Portable content paths, not native filesystem authority.

#![deny(unnameable_test_items)]

mod support;

use fabric_sdk_sql_definition::{
    DefinitionErrorKind as Kind, SqlDatabaseDefinitionFormat as Format,
    parse_sql_database_definition as parse, sql_database_definition_from_parts as from_parts,
};
use serde_json::json;
use support::{assert_kind, limits, part, wire_part};

#[test]
fn unsafe_native_paths_fail_without_trimming_decoding_or_replacement() {
    for path in [
        "",
        "/a.dacpac",
        "//server/a.dacpac",
        "../a.dacpac",
        "folder/../a.dacpac",
        "./a.dacpac",
        "folder/./a.dacpac",
        "folder//a.dacpac",
        "a.dacpac/",
        "C:/a.dacpac",
        "C:a.dacpac",
        "\\\\server\\a.dacpac",
        "\\\\?\\C:\\a.dacpac",
        "\\\\.\\device.dacpac",
        "folder\\a.dacpac",
        "a.dacpac:stream",
        "a\0.dacpac",
        "a\n.dacpac",
        "a\t.dacpac",
        "a\u{7f}.dacpac",
        "bad?/a.dacpac",
        "bad*/a.dacpac",
        "bad\"/a.dacpac",
        "bad</a.dacpac",
        "bad>/a.dacpac",
        "bad|/a.dacpac",
        "folder./a.dacpac",
        "folder /a.dacpac",
        "a.dacpac.",
        "a.dacpac ",
    ] {
        assert_kind(
            from_parts(Format::Dacpac, vec![part(path, b"x")], limits()),
            Kind::UnsafePath,
        );
        let body = json!({"parts": [wire_part(path, b"x")]});
        assert_kind(
            parse(&serde_json::to_vec(&body).unwrap(), limits()),
            Kind::UnsafePath,
        );
    }
}

#[test]
fn windows_reserved_components_are_rejected_on_every_platform() {
    for component in [
        "CON", "con.txt", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$", "COM1", "COM9", "LPT1",
        "lpt9.txt", "COM¹", "LPT²", "LPT³", "CON .txt",
    ] {
        assert_kind(
            from_parts(
                Format::Dacpac,
                vec![part(&format!("{component}/a.dacpac"), b"x")],
                limits(),
            ),
            Kind::UnsafePath,
        );
    }
    for path in ["COM10.dacpac", "LPT0.dacpac", "CONSOLE.dacpac"] {
        from_parts(Format::Dacpac, vec![part(path, b"x")], limits()).unwrap();
    }
}

#[test]
fn duplicates_are_errors_even_when_bytes_match() {
    for second_bytes in [&b"first"[..], &b"different"[..]] {
        assert_kind(
            from_parts(
                Format::Sqlproj,
                vec![
                    part("a.sqlproj", b"project"),
                    part("dbo/a.sql", b"first"),
                    part("dbo/a.sql", second_bytes),
                ],
                limits(),
            ),
            Kind::DuplicatePath,
        );
    }
    let body = json!({"format": "sqlproj", "parts": [
        wire_part("a.sqlproj", b"project"),
        wire_part("a.sql", b"first"),
        wire_part("a.sql", b"second")
    ]});
    let error = parse(&serde_json::to_vec(&body).unwrap(), limits()).unwrap_err();
    assert_eq!(error.kind, Kind::DuplicatePath);
    assert_eq!(error.part_index, Some(2));
}

#[test]
fn full_and_parent_case_aliases_fail_in_both_orders() {
    for (first, second) in [
        ("a.sql", "A.sql"),
        ("dbo/a.sql", "DBO/b.sql"),
        ("dbo/Tables/a.sql", "dbo/tables/b.sql"),
        ("é/a.sql", "É/b.sql"),
    ] {
        for paths in [[first, second], [second, first]] {
            assert_kind(
                from_parts(
                    Format::Sqlproj,
                    vec![
                        part("a.sqlproj", b"project"),
                        part(paths[0], b"x"),
                        part(paths[1], b"y"),
                    ],
                    limits(),
                ),
                Kind::CaseCollision,
            );
        }
    }
}

#[test]
fn files_cannot_also_be_parent_directories_in_either_order() {
    for paths in [["a.sql", "a.sql/child.sql"], ["a.sql/child.sql", "a.sql"]] {
        assert_kind(
            from_parts(
                Format::Sqlproj,
                vec![
                    part("project.sqlproj", b"project"),
                    part(paths[0], b"x"),
                    part(paths[1], b"y"),
                ],
                limits(),
            ),
            Kind::FileDirectoryCollision,
        );
    }
}

#[test]
fn paths_stay_native_without_unicode_or_percent_normalization() {
    let definition = from_parts(
        Format::Sqlproj,
        vec![
            part("nested/项目.sqlproj", b"project"),
            part("é.sql", b"composed"),
            part("e\u{301}.sql", b"decomposed"),
            part("%2e%2e/query.sql", b"literal percent bytes"),
            part(".sharedqueries/query.sql", b"shared"),
        ],
        limits(),
    )
    .unwrap();
    assert_eq!(definition.parts()[0].path, "nested/项目.sqlproj");
    assert_ne!(definition.parts()[1].path, definition.parts()[2].path);
    assert_eq!(definition.parts()[3].path, "%2e%2e/query.sql");
    let again = parse(&serde_json::to_vec(&definition).unwrap(), limits()).unwrap();
    assert_eq!(again.parts(), definition.parts());
}
