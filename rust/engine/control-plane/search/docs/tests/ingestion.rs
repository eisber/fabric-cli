// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline committed-source ingestion and bundle contracts.

#![cfg(feature = "index-builder")]
#![deny(unnameable_test_items)]

use std::{
    collections::BTreeMap,
    error::Error,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use fabric_docs_search::{
    BundleManifest, DocsSearch, SearchOptions,
    builder::{build, build_with_features},
    rich::{IndexFeatures, Metadata},
};
use tantivy::{
    Index, ReloadPolicy, TantivyDocument, Term,
    collector::{Count, TopDocs},
    directory::MmapDirectory,
    query::{AllQuery, TermQuery},
    schema::{IndexRecordOption, Value},
};
use tempfile::TempDir;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;
type Record = BTreeMap<String, String>;

struct Fixture {
    directory: TempDir,
    source: PathBuf,
    revision: String,
}

impl Fixture {
    fn new(files: &[(&str, &str)]) -> TestResult<Self> {
        let directory = tempfile::Builder::new()
            .prefix(".fabric-docs-ingestion-")
            .tempdir_in(std::env::current_dir()?)?;
        let source = directory.path().join("source");
        fs::create_dir(&source)?;
        for (path, text) in [
            ("LICENSE", "Synthetic documentation license\n"),
            ("LICENSE-CODE", "Synthetic code license\n"),
            ("ThirdPartyNotices.md", "Synthetic third-party notice\n"),
            (
                "docs/docfx.json",
                r#"{"build":{"content":[{
                "src":"./","files":["**/*.md","**/*.yml","**/*.yaml"],
                "exclude":["**/includes/**","**/obj/**","**/drafts/**"]
            }]}}"#,
            ),
        ]
        .into_iter()
        .chain(files.iter().copied())
        {
            write(&source, path, text)?;
        }
        let mut fixture = Self {
            directory,
            source,
            revision: String::new(),
        };
        fixture.git(&["init", "--quiet"])?;
        fixture.commit_all()?;
        Ok(fixture)
    }

    fn git(&self, arguments: &[&str]) -> TestResult<String> {
        let result = Command::new("git")
            .args(["--no-pager", "-C"])
            .arg(&self.source)
            .args([
                "-c",
                "user.name=Fabric Docs Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.hooksPath=.fixture-no-hooks",
                "-c",
                "core.autocrlf=false",
                "-c",
                "core.longpaths=true",
            ])
            .env("GIT_AUTHOR_NAME", "Fabric Docs Fixture")
            .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
            .env("GIT_COMMITTER_NAME", "Fabric Docs Fixture")
            .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
            .args(arguments)
            .output()?;
        if !result.status.success() {
            return Err(format!(
                "fixture Git failed: {}",
                String::from_utf8_lossy(&result.stderr)
            )
            .into());
        }
        Ok(String::from_utf8(result.stdout)?.trim().to_owned())
    }

    fn commit_all(&mut self) -> TestResult {
        self.git(&["add", "--all"])?;
        self.commit_index()
    }

    fn commit_index(&mut self) -> TestResult {
        self.git(&[
            "commit",
            "--quiet",
            "-m",
            "Synthetic offline ingestion fixture",
        ])?;
        self.revision = self.git(&["rev-parse", "HEAD"])?;
        Ok(())
    }

    fn output(&self, name: &str) -> PathBuf {
        self.directory.path().join(name)
    }

    fn build(&self, name: &str, compression: &str) -> TestResult<BundleManifest> {
        build(
            &self.source,
            &self.output(name),
            &self.revision,
            compression,
        )
    }

    fn build_profile(&self, name: &str) -> TestResult<BundleManifest> {
        build_with_features(
            &self.source,
            &self.output(name),
            &self.revision,
            "none",
            IndexFeatures::profile(name).ok_or("unknown fixture profile")?,
        )
    }
}

fn write(root: &Path, path: &str, text: &str) -> TestResult {
    let path = root.join(path);
    fs::create_dir_all(path.parent().ok_or("fixture file has no parent")?)?;
    fs::write(path, text)?;
    Ok(())
}

fn records(path: &Path) -> TestResult<Vec<Record>> {
    let index = Index::open_in_dir(path)?;
    let reader: tantivy::IndexReader = index
        .reader_builder()
        .reload_policy(ReloadPolicy::Manual)
        .try_into()?;
    let searcher = reader.searcher();
    let matches = searcher.search(&AllQuery, &TopDocs::with_limit(100).order_by_score())?;
    let mut records = Vec::new();
    for (_, address) in matches {
        let document: TantivyDocument = searcher.doc(address)?;
        let mut record = BTreeMap::new();
        for name in [
            "id",
            "title",
            "heading",
            "body",
            "workload",
            "url",
            "source_url",
            "source_path",
        ] {
            let value = document
                .get_first(index.schema().get_field(name)?)
                .and_then(|value| value.as_str())
                .ok_or("missing stored field")?;
            record.insert(name.to_owned(), value.to_owned());
        }
        for name in ["description", "aliases", "metadata_json", "heading_path"] {
            if let Some(value) = document
                .get_first(index.schema().get_field(name)?)
                .and_then(|value| value.as_str())
            {
                record.insert(name.to_owned(), value.to_owned());
            }
        }
        records.push(record);
    }
    records.sort_by(|left, right| left["id"].cmp(&right["id"]));
    Ok(records)
}

fn term_count(path: &Path, field: &str, text: &str) -> TestResult<usize> {
    let index = Index::open_in_dir(path)?;
    let reader: tantivy::IndexReader = index
        .reader_builder()
        .reload_policy(ReloadPolicy::Manual)
        .try_into()?;
    let query = TermQuery::new(
        Term::from_field_text(index.schema().get_field(field)?, text),
        IndexRecordOption::Basic,
    );
    Ok(reader.searcher().search(&query, &Count)?)
}

fn metadata(record: &Record) -> TestResult<Metadata> {
    Ok(serde_json::from_str(&record["metadata_json"])?)
}

fn legacy_records(records: Vec<Record>) -> Vec<Record> {
    records
        .into_iter()
        .map(|mut record| {
            for name in ["description", "aliases", "metadata_json", "heading_path"] {
                record.remove(name);
            }
            record
        })
        .collect()
}

#[test]
fn headings_frontmatter_unicode_code_links_and_images() -> TestResult {
    let fixture = Fixture::new(&[(
        "docs/onelake/article.md",
        r#"---
title: "Fabric **Unicode** guide"
author: never-index-this-author
ms.date: never-index-this-date
---
# 設定 🚀 Café

Useful [friendly link](https://example.invalid/never-index-link-destination).
![never-index-image-alt](data:image/png;base64,never-index-image-payload)

:::image type="content" source="never-index-directive-path.png" alt-text="never-index-directive-alt":::

:::image type="complex" source="never-index-complex.png" alt-text="invisible":::
never-index-complex-description
:::image-end:::

<img src="never-index-html-image.png" alt="never-index-html-alt">
<!-- never-index-comment [!INCLUDE [ignored](missing-comment.md)] -->

## Code and SQL

```python
  spark.sql("SELECT * FROM Table_Name")
  preserved_indentation = True
[!INCLUDE [literal](missing-code.md)]
:::image type="content" source="literal-code.png":::
```
"#,
    )])?;
    let manifest = fixture.build("index", "lz4")?;
    let output = fixture.output("index");
    let records = records(&output)?;
    assert_eq!(manifest.documents, 2);
    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["id"], "docs/onelake/article.md#section-1");
    assert_eq!(records[0]["title"], "Fabric Unicode guide");
    assert_eq!(records[0]["heading"], "設定 🚀 Café");
    assert!(records[0]["body"].contains("Useful friendly link"));
    let text = records
        .iter()
        .map(|record| record["body"].as_str())
        .collect::<String>();
    assert!(!text.contains("never-index"));
    assert!(text.contains("spark.sql(\"SELECT * FROM Table_Name\")"));
    assert!(records[1]["body"].starts_with("  spark.sql"));
    assert!(text.contains("  preserved_indentation = True"));
    assert!(text.contains("missing-code.md"));
    assert!(text.contains("literal-code.png"));
    assert_eq!(records[1]["heading"], "Code and SQL");
    assert_eq!(
        records[1]["url"],
        "https://learn.microsoft.com/fabric/onelake/article"
    );
    assert_eq!(records[1]["workload"], "onelake");
    assert_eq!(
        records[1]["source_url"],
        format!(
            "https://github.com/MicrosoftDocs/fabric-docs/blob/{}/docs/onelake/article.md",
            fixture.revision
        )
    );
    let search = DocsSearch::open(MmapDirectory::open(output)?, manifest)?;
    let hits = search.search(&SearchOptions {
        text: "identifiers:Table_Name".to_owned(),
        limit: 5,
        workload: Some("onelake".to_owned()),
        fuzzy: 0,
    })?;
    assert_eq!(hits.data.len(), 1);
    Ok(())
}

#[test]
fn local_nested_includes_and_external_dependencies_are_distinguished() -> TestResult {
    let fixture = Fixture::new(&[
        (
            "docs/onelake/article.md",
            "---\ntitle: Includes\n---\n# Includes\n\n[!INCLUDE [shared](../fragments//shared.md)]\n\n[!INCLUDE [external](~/..//powerbi-repo/docs/shared.md)]\n[!INCLUDE [external](../../kusto-repo/docs/shared.md)]\n[!INCLUDE [external](../../powerquery-repo/docs/shared.md)]\n[!INCLUDE [web](https://example.invalid/include.md)]\n",
        ),
        (
            "docs/fragments/shared.md",
            "---\nauthor: excluded-include-author\n---\nLocal shared content.\n\n[!include [nested](~/includes/nested.md)]\n",
        ),
        (
            "docs/includes/nested.md",
            "Nested Ω text and `OneLake.Path`.\n",
        ),
        (
            "docs/toc.yml",
            "- name: navigation-only\n  href: onelake/article.md\n",
        ),
        ("docs/README.md", "# Never standalone\n"),
    ])?;
    let manifest = fixture.build("index", "none")?;
    let records = records(&fixture.output("index"))?;
    assert_eq!(records.len(), 1);
    assert!(records[0]["body"].contains("Local shared content."));
    assert!(records[0]["body"].contains("Nested Ω text and OneLake.Path."));
    assert!(!records[0]["body"].contains("excluded-include-author"));
    assert!(!records[0]["body"].contains("INCLUDE"));
    assert_eq!(manifest.unresolved_includes.len(), 4);
    assert!(
        manifest
            .unresolved_includes
            .iter()
            .any(|entry| entry.starts_with("docs/onelake/article.md:8:"))
    );
    assert!(
        manifest
            .unresolved_includes
            .iter()
            .any(|entry| entry.contains("powerquery-repo"))
    );
    for path in [
        "docs/fragments/shared.md",
        "docs/includes/nested.md",
        "docs/toc.yml",
        "docs/README.md",
    ] {
        assert!(
            manifest
                .skipped_inputs
                .iter()
                .any(|entry| entry.starts_with(path))
        );
    }
    assert_eq!(manifest.source_markdown_files, 4);
    Ok(())
}

#[test]
fn yaml_landing_text_and_docfx_source_exclusions() -> TestResult {
    let fixture = Fixture::new(&[
        (
            "docs/onelake/index.yml",
            r#"### YamlMime:Landing
title: OneLake landing
summary: Explore unified storage.
metadata:
  author: never-index-landing-author
  description: never-index-metadata-description
landingContent:
  - title: Connect with Unicode 東京
    linkLists:
      - linkListType: overview
        links:
          - text: Read **OneLake** documentation
            url: https://example.invalid/never-index-landing-url
    imageSrc: never-index-image-source.png
"#,
        ),
        (
            "docs/index.yml",
            "### YamlMime:Hub\ntitle: Fabric hub\nsummary: Fabric overview.\n",
        ),
        (
            "docs/onelake/toc.yml",
            "### YamlMime:TOC\n- name: never-index-navigation\n  href: page.md\n",
        ),
        (
            "docs/onelake/config.yml",
            "metadata:\n  title: not-a-landing-page\n",
        ),
        ("docs/onelake/drafts/hidden.md", "# never-index-draft\n"),
        ("docs/obj/generated.md", "# never-index-object\n"),
        ("docs/.github/guide.md", "# never-index-github\n"),
        ("docs/onelake/README.md", "# never-index-readme\n"),
    ])?;
    let manifest = fixture.build("index", "zstd")?;
    let records = records(&fixture.output("index"))?;
    assert_eq!(records.len(), 2);
    let landing = records
        .iter()
        .find(|record| record["source_path"] == "docs/onelake/index.yml")
        .ok_or("missing landing record")?;
    assert_eq!(landing["title"], "OneLake landing");
    assert_eq!(
        landing["url"],
        "https://learn.microsoft.com/fabric/onelake/"
    );
    assert!(landing["body"].contains("Connect with Unicode 東京"));
    assert!(landing["body"].contains("Read OneLake documentation"));
    assert!(!landing["body"].contains("never-index"));
    assert!(
        manifest
            .skipped_inputs
            .iter()
            .any(|entry| entry == "docs/onelake/drafts/hidden.md: DocFX source exclusion")
    );
    assert!(
        manifest
            .skipped_inputs
            .iter()
            .any(|entry| entry == "docs/onelake/config.yml: not a YAML landing page")
    );
    assert!(
        records
            .iter()
            .any(|record| record["url"] == "https://learn.microsoft.com/fabric/")
    );
    Ok(())
}

#[test]
fn committed_bytes_stable_ids_and_finalized_component_checksums() -> TestResult {
    let fixture = Fixture::new(&[(
        "docs/onelake/article.md",
        "# Repeated\nCommitted Café.\n\n## Repeated\nSecond section.\n",
    )])?;
    let first = fixture.build("first", "lz4")?;
    write(
        &fixture.source,
        "docs/onelake/article.md",
        "# Mutable edit\nNever index working-tree edits.\n",
    )?;
    write(
        &fixture.source,
        "docs/onelake/untracked.md",
        "# Never index untracked files\n",
    )?;
    let second = fixture.build("second", "lz4")?;
    let first_records = records(&fixture.output("first"))?;
    let second_records = records(&fixture.output("second"))?;
    assert_eq!(first_records, second_records);
    assert_ne!(first_records[0]["id"], first_records[1]["id"]);
    assert_eq!(first_records[0]["heading"], first_records[1]["heading"]);
    assert!(second_records[0]["body"].contains("Committed Café."));
    assert_eq!(first.source_markdown_files, 1);
    assert_eq!(
        first.source_markdown_bytes,
        "# Repeated\nCommitted Café.\n\n## Repeated\nSecond section.\n".len() as u64
    );
    assert_eq!(first.documents, second.documents);
    assert_eq!(first.segments, 1);
    let names = first
        .files
        .iter()
        .map(|file| file.name.as_str())
        .collect::<Vec<_>>();
    let mut sorted = names.clone();
    sorted.sort_unstable();
    assert_eq!(names, sorted);
    assert!(names.contains(&"meta.json"));
    assert!(
        !names
            .iter()
            .any(|name| name.ends_with(".lock") || name.ends_with(".temp"))
    );
    for component in &first.files {
        let bytes = fs::read(fixture.output("first").join(&component.name))?;
        assert_eq!(component.bytes, bytes.len() as u64);
        let hash = ring::digest::digest(&ring::digest::SHA256, &bytes)
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        assert_eq!(component.sha256, hash);
    }
    for name in [
        "LICENSE",
        "LICENSE-CODE",
        "ThirdPartyNotices.md",
        "TRANSFORMATION-ATTRIBUTION",
    ] {
        assert!(
            first
                .licenses
                .iter()
                .any(|notice| notice.name == name && !notice.text.is_empty())
        );
    }
    let persisted: BundleManifest =
        serde_json::from_slice(&fs::read(fixture.output("first").join("bundle.json"))?)?;
    assert_eq!(persisted.source_commit, fixture.revision);
    assert_eq!(persisted.documents, first.documents);
    assert_eq!(persisted.files.len(), first.files.len());
    Ok(())
}

#[test]
fn includes_reject_cycles_missing_local_paths_and_traversal() -> TestResult {
    for target in [
        "../../../outside.md",
        "/absolute.md",
        "C:/outside.md",
        "..\\outside.md",
        "%2e%2e/outside.md",
        "missing.md",
        "article.md",
    ] {
        let markdown = format!("# Article\n[!INCLUDE [unsafe]({target})]\n");
        let fixture = Fixture::new(&[("docs/onelake/article.md", &markdown)])?;
        assert!(
            fixture.build("rejected", "none").is_err(),
            "accepted {target}"
        );
        assert!(!fixture.output("rejected").join("bundle.json").exists());
    }
    let fixture = Fixture::new(&[
        (
            "docs/onelake/article.md",
            "# Article\n[!INCLUDE [a](../includes/a.md)]\n",
        ),
        ("docs/includes/a.md", "[!INCLUDE [b](b.md)]\n"),
        ("docs/includes/b.md", "[!INCLUDE [a](a.md)]\n"),
    ])?;
    let error = fixture
        .build("cycle", "none")
        .err()
        .ok_or("cycle accepted")?
        .to_string();
    assert!(error.contains("cycle"));
    Ok(())
}

#[test]
fn includes_reject_symlink_objects_and_excessive_depth() -> TestResult {
    let mut fixture = Fixture::new(&[
        (
            "docs/onelake/article.md",
            "# Article\n[!INCLUDE [link](../includes/link.md)]\n",
        ),
        ("docs/includes/link.md", "../../outside.md"),
    ])?;
    let object = fixture.git(&["rev-parse", "HEAD:docs/includes/link.md"])?;
    fixture.git(&[
        "update-index",
        "--cacheinfo",
        &format!("120000,{object},docs/includes/link.md"),
    ])?;
    fixture.commit_index()?;
    let error = fixture
        .build("symlink", "none")
        .err()
        .ok_or("symlink accepted")?
        .to_string();
    assert!(error.contains("symlink"));

    let mut fixture = Fixture::new(&[(
        "docs/onelake/article.md",
        "# Article\n[!INCLUDE [deep](../includes/0.md)]\n",
    )])?;
    for depth in 0..34 {
        write(
            &fixture.source,
            &format!("docs/includes/{depth}.md"),
            &format!("[!INCLUDE [next]({}.md)]\n", depth + 1),
        )?;
    }
    write(&fixture.source, "docs/includes/34.md", "Too deep.\n")?;
    fixture.commit_all()?;
    let error = fixture
        .build("deep", "none")
        .err()
        .ok_or("unbounded include depth")?
        .to_string();
    assert!(error.contains("depth"));
    Ok(())
}

#[test]
fn rejects_invalid_pins_compression_paths_and_existing_output() -> TestResult {
    let fixture = Fixture::new(&[("docs/onelake/article.md", "# Article\nText.\n")])?;
    for revision in ["main", "", "0000000000000000000000000000000000000000"] {
        assert!(build(&fixture.source, &fixture.output("pin"), revision, "none").is_err());
    }
    assert!(fixture.build("codec", "brotli").is_err());
    assert!(
        build(
            Path::new("relative"),
            &fixture.output("relative"),
            &fixture.revision,
            "none"
        )
        .is_err()
    );
    assert!(
        build(
            &fixture.source,
            Path::new("relative"),
            &fixture.revision,
            "none"
        )
        .is_err()
    );
    assert!(
        build(
            &fixture.source,
            &fixture.source.join("nested-output"),
            &fixture.revision,
            "none"
        )
        .is_err()
    );
    let existing = fixture.output("existing");
    fs::create_dir(&existing)?;
    fs::write(existing.join("owner-file"), "must remain untouched")?;
    assert!(fixture.build("existing", "none").is_err());
    assert_eq!(
        fs::read_to_string(existing.join("owner-file"))?,
        "must remain untouched"
    );
    assert_eq!(fs::read_dir(existing)?.count(), 1);
    Ok(())
}

#[test]
fn malformed_frontmatter_and_empty_corpus_do_not_publish_a_manifest() -> TestResult {
    let fixture = Fixture::new(&[("docs/onelake/article.md", "---\ntitle: unclosed\n# Text\n")])?;
    assert!(fixture.build("frontmatter", "none").is_err());
    assert!(!fixture.output("frontmatter").join("bundle.json").exists());
    let fixture = Fixture::new(&[("docs/toc.yml", "- name: navigation only\n")])?;
    assert!(fixture.build("empty", "none").is_err());
    assert!(!fixture.output("empty").join("bundle.json").exists());
    Ok(())
}

#[test]
fn metadata_inherits_docfx_values_and_article_overrides_without_inference() -> TestResult {
    let fixture = Fixture::new(&[
        (
            "docs/docfx.json",
            r#"{"build":{
                "content":[{"files":["**/*.md","**/*.yml"]}],
                "globalMetadata":{
                    "author":"global-author", "description":"Global **description**.",
                    "ms.search.form":"inherited, shared", "ms.topic":["overview","conceptual"],
                    "enabled":true, "priority":1.25, "product":"Fabric",
                    "_private":"private-value", "imageSrc":"hidden.png",
                    "nested":{"key":"not-public"}, "arrayNested":[{"key":"not-public"}]
                },
                "fileMetadata":{
                    "author":{"onelake/**/*.md":"file-author"},
                    "description":{"onelake/**/*.md":"File description."},
                    "ms.search.form":{"onelake/**/*.md":"file-alias, old"},
                    "ms.topic":{"**/*.md":["guide"],"onelake/**/*.md":["tutorial","how-to"]}
                }
            }}"#,
        ),
        (
            "docs/onelake/a.md",
            r#"---
title: Authored A
description: Authored **description**.
author: article-author
aliases: ["Café alias", "Alias, with comma"]
ms.search.form: "self-serve, Ω alias, self-serve"
ms.topic: [tutorial, example, tutorial]
ms.date: 2026-10-01
enabled: false
count: 42
item_type: Lakehouse
itemType: [Lakehouse, Warehouse]
ms.item_type: Notebook
---
# Root
Lakehouse is an explicit item type on this article.
## Child
Second section, still one article.
"#,
        ),
        (
            "docs/warehouse/b.md",
            "---\ntitle: Authored B\nauthor: null\n---\n# Warehouse\nA Lakehouse mention must not infer an item type.\n",
        ),
        (
            "docs/onelake/c.md",
            "# File metadata\nFile values override globals when the author is silent.\n",
        ),
        (
            "docs/onelake/index.yml",
            r#"### YamlMime:Landing
title: Authored landing
metadata:
  author: landing-author
  description: Landing *description*.
  ms.topic: [landing]
  ms.search.form: ["Landing alias", "Second alias"]
landingContent:
  - title: Connect
    imageSrc: hidden-image.png
    linkLists:
      - links:
          - text: Landing content.
            url: https://example.invalid/hidden
"#,
        ),
    ])?;
    // Neither configuration nor article metadata may come from mutable checkout bytes.
    write(
        &fixture.source,
        "docs/docfx.json",
        r#"{"build":{"content":[],"globalMetadata":{"product":"mutable"}}}"#,
    )?;
    write(
        &fixture.source,
        "docs/onelake/a.md",
        "---\ntitle: Mutable\nitem_type: Mutable\n---\n# Mutable\n",
    )?;
    let manifest = fixture.build("index", "none")?;
    assert_eq!(manifest.features, IndexFeatures::full());
    assert_eq!(manifest.documents, 5);
    let output = fixture.output("index");
    let records = records(&output)?;
    let authored = records
        .iter()
        .find(|record| record["source_path"] == "docs/onelake/a.md")
        .ok_or("missing authored article")?;
    let values = metadata(authored)?;
    assert_eq!(values["author"], ["article-author"]);
    assert_eq!(values["ms.topic"], ["tutorial", "example"]);
    assert_eq!(values["ms.date"], ["2026-10-01"]);
    assert_eq!(values["enabled"], ["false"]);
    assert_eq!(values["count"], ["42"]);
    assert_eq!(values["priority"], ["1.25"]);
    assert_eq!(values["item_type"], ["Lakehouse"]);
    assert_eq!(values["itemType"], ["Lakehouse", "Warehouse"]);
    assert_eq!(values["ms.item_type"], ["Notebook"]);
    assert_eq!(
        values["ms.search.form"],
        ["self-serve, Ω alias, self-serve"]
    );
    assert_eq!(values["aliases"], ["Café alias", "Alias, with comma"]);
    assert_eq!(values["source_path"], ["docs/onelake/a.md"]);
    assert_eq!(values["workload"], ["onelake"]);
    assert_eq!(authored["description"], "Authored description.");
    for alias in ["Café alias", "Alias", "with comma", "self-serve", "Ω alias"] {
        assert!(authored["aliases"].lines().any(|value| value == alias));
    }
    assert!(!authored["aliases"].contains("file-alias"));
    for field in ["_private", "imageSrc", "nested", "arrayNested"] {
        assert!(!values.contains_key(field), "retained {field}");
        assert!(!manifest.filter_catalog.contains_key(field));
    }
    let absent = records
        .iter()
        .find(|record| record["source_path"] == "docs/warehouse/b.md")
        .ok_or("missing article without item types")?;
    let absent_metadata = metadata(absent)?;
    for field in ["author", "item_type", "itemType", "ms.item_type"] {
        assert!(!absent_metadata.contains_key(field), "inferred {field}");
    }
    assert_eq!(absent_metadata["ms.topic"], ["guide"]);
    assert_eq!(absent_metadata["enabled"], ["true"]);
    assert_eq!(absent["description"], "Global description.");
    let file = records
        .iter()
        .find(|record| record["source_path"] == "docs/onelake/c.md")
        .ok_or("missing file-metadata article")?;
    assert_eq!(metadata(file)?["author"], ["file-author"]);
    assert_eq!(metadata(file)?["ms.topic"], ["guide", "tutorial", "how-to"]);
    assert_eq!(file["description"], "File description.");
    let landing = records
        .iter()
        .find(|record| record["source_path"] == "docs/onelake/index.yml")
        .ok_or("missing landing article")?;
    assert_eq!(metadata(landing)?["author"], ["landing-author"]);
    assert_eq!(metadata(landing)?["ms.topic"], ["landing"]);
    assert_eq!(landing["description"], "Landing description.");
    assert!(landing["aliases"].contains("Landing alias"));
    assert!(!landing["body"].contains("hidden"));
    assert_eq!(manifest.filter_catalog["product"]["Fabric"], 4);
    assert_eq!(manifest.filter_catalog["author"]["article-author"], 1);
    assert_eq!(manifest.filter_catalog["ms.topic"]["tutorial"], 2);
    assert_eq!(manifest.filter_catalog["ms.topic"]["guide"], 2);
    assert_eq!(manifest.filter_catalog["item_type"]["Lakehouse"], 1);
    assert_eq!(manifest.filter_catalog["itemType"]["Warehouse"], 1);
    assert_eq!(
        manifest.filter_catalog["ms.search.form"]["self-serve, Ω alias, self-serve"],
        1
    );
    assert!(!manifest.filter_catalog["ms.search.form"].contains_key("self-serve"));
    assert_eq!(manifest.filter_catalog["workload"]["onelake"], 3);
    assert_eq!(
        manifest.filter_catalog["source_path"]["docs/onelake/a.md"],
        1
    );
    assert_eq!(term_count(&output, "metadata_terms", "product=Fabric")?, 5);
    assert_eq!(
        term_count(&output, "metadata_terms", "author=article-author")?,
        2
    );
    assert_eq!(
        term_count(&output, "metadata_terms", "author=global-author")?,
        0
    );
    assert_eq!(
        term_count(&output, "metadata_terms", "item_type=lakehouse")?,
        0
    );
    assert_eq!(term_count(&output, "aliases", "café")?, 2);
    assert_eq!(term_count(&output, "description", "authored")?, 2);
    Ok(())
}

#[test]
fn raw_alias_metadata_stays_exact_and_explicit_search_hints_are_lexical() -> TestResult {
    let fixture = Fixture::new(&[(
        "docs/onelake/aliases.md",
        "---\ntitle: Ordinary title\naliases: 'first, second'\nms.search.form: 'third, fourth'\nms.search: [zephyrhint, 'Nova,  price']\nsearch.app: routinglabel\n---\n# Ordinary heading\nOrdinary body.\n",
    )])?;
    let manifest = fixture.build("index", "none")?;
    let output = fixture.output("index");
    let records = records(&output)?;
    let values = metadata(&records[0])?;
    assert_eq!(values["aliases"], ["first, second"]);
    assert_eq!(values["ms.search.form"], ["third, fourth"]);
    assert_eq!(values["ms.search"], ["zephyrhint", "Nova,  price"]);
    assert_eq!(manifest.filter_catalog["aliases"]["first, second"], 1);
    assert_eq!(
        manifest.filter_catalog["ms.search.form"]["third, fourth"],
        1
    );
    assert_eq!(manifest.filter_catalog["ms.search"]["Nova,  price"], 1);
    assert_eq!(
        term_count(&output, "metadata_terms", "aliases=first, second")?,
        1
    );
    assert_eq!(term_count(&output, "metadata_terms", "aliases=first")?, 0);
    for term in [
        "first",
        "second",
        "third",
        "fourth",
        "zephyrhint",
        "nova",
        "price",
    ] {
        assert_eq!(term_count(&output, "aliases", term)?, 1, "{term}");
    }
    for field in ["title", "heading", "body", "description"] {
        assert_eq!(term_count(&output, field, "zephyrhint")?, 0, "{field}");
    }
    assert_eq!(term_count(&output, "aliases", "routinglabel")?, 0);
    assert_eq!(
        term_count(&output, "metadata_terms", "search.app=routinglabel")?,
        1
    );
    Ok(())
}

#[test]
fn metadata_is_bounded_and_only_public_scalar_values_are_exposed() -> TestResult {
    let invalid_key = "x".repeat(81);
    let values = (0..254)
        .map(|index| format!("value-{index}"))
        .collect::<Vec<_>>()
        .join(", ");
    let unsafe_metadata = format!(
        "---\ntitle: Scalar metadata\nvalid: [true, false, 42, 1.5, '2026-01-02', 'Exact Case']\n\
         accepted: '{}'\ntoo_long: '{}'\n{invalid_key}: rejected\n\
         slash/key: rejected\n_private: rejected\n.hidden: rejected\n\
         imageSrc: hidden.png\nms.social_image_url: hidden.png\n\
         nested: {{key: rejected}}\nmixed: [valid, {{key: rejected}}]\n\
         infinite: .inf\nempty: ''\nsource_path: forged\nworkload: forged\n\
         # comment-only: never indexed\n---\n# Article\nBody.\n",
        "a".repeat(512),
        "b".repeat(513),
    );
    let bounded = format!("---\nmany: [{values}]\n---\n# Bounded\nBody.\n");
    let fixture = Fixture::new(&[
        ("docs/onelake/scalars.md", &unsafe_metadata),
        ("docs/onelake/bounded.md", &bounded),
    ])?;
    let manifest = fixture.build("index", "none")?;
    let records = records(&fixture.output("index"))?;
    let scalar = records
        .iter()
        .find(|record| record["source_path"] == "docs/onelake/scalars.md")
        .ok_or("missing scalar metadata")?;
    let metadata = metadata(scalar)?;
    assert_eq!(
        metadata["valid"],
        ["true", "false", "42", "1.5", "2026-01-02", "Exact Case"]
    );
    assert_eq!(metadata["accepted"], ["a".repeat(512)]);
    for field in [
        "too_long",
        &invalid_key,
        "slash/key",
        "_private",
        ".hidden",
        "imageSrc",
        "ms.social_image_url",
        "nested",
        "mixed",
        "infinite",
        "empty",
        "comment-only",
    ] {
        assert!(!metadata.contains_key(field), "retained {field}");
        assert!(!manifest.filter_catalog.contains_key(field));
    }
    assert_eq!(metadata["workload"], ["onelake"]);
    assert_eq!(metadata["source_path"], ["docs/onelake/scalars.md"]);
    let bounded = records
        .iter()
        .find(|record| record["source_path"] == "docs/onelake/bounded.md")
        .ok_or("missing bounded metadata")?;
    let bounded: Metadata = serde_json::from_str(&bounded["metadata_json"])?;
    assert_eq!(bounded.values().map(Vec::len).sum::<usize>(), 256);
    assert_eq!(bounded["many"].len(), 254);
    assert!(!manifest.filter_catalog.contains_key("item_type"));
    assert!(!manifest.filter_catalog.contains_key("itemType"));
    assert!(!manifest.filter_catalog.contains_key("ms.item_type"));
    let excessive = format!("---\nmany: [{values}, one-too-many]\n---\n# Excessive\nBody.\n");
    let fixture = Fixture::new(&[("docs/onelake/excessive.md", &excessive)])?;
    let error = fixture
        .build("rejected", "none")
        .err()
        .ok_or("accepted an incomplete metadata vocabulary")?
        .to_string();
    assert!(error.contains("metadata exceeds 256 values"));
    assert!(!fixture.output("rejected").join("bundle.json").exists());
    Ok(())
}

#[test]
fn hierarchy_tracks_source_levels_and_links_only_explicit_unicode_anchors() -> TestResult {
    let fixture = Fixture::new(&[(
        "docs/onelake/hierarchy.md",
        r#"---
title: Distinct authored title
---
Preamble before the first source heading.
# Root {#root-id}
Parent body is not copied into descendants.
### Grandchild Ω
```markdown
## This is code, not a section
```
::: zone target="docs"
Conditional body stays in the same section.
::: zone-end
## Sibling Café {#設定}
Sibling body.
# Next root
Next body.
"#,
    )])?;
    let manifest = fixture.build_profile("hierarchy")?;
    assert_eq!(manifest.documents, 5);
    let output = fixture.output("hierarchy");
    let rich = records(&output)?;
    let expected = [
        Vec::<&str>::new(),
        vec!["Root"],
        vec!["Root", "Grandchild Ω"],
        vec!["Root", "Sibling Café"],
        vec!["Next root"],
    ];
    let base = "https://learn.microsoft.com/fabric/onelake/hierarchy";
    for (ordinal, (record, path)) in rich.iter().zip(expected).enumerate() {
        let actual: Vec<String> = serde_json::from_str(&record["heading_path"])?;
        assert_eq!(actual, path);
        assert_eq!(
            record["id"],
            format!("docs/onelake/hierarchy.md#section-{ordinal}")
        );
        assert_eq!(record["title"], "Distinct authored title");
    }
    assert_eq!(rich[0]["url"], base);
    assert_eq!(rich[1]["url"], format!("{base}#root-id"));
    assert_eq!(rich[2]["url"], base);
    assert_eq!(rich[3]["url"], format!("{base}#%E8%A8%AD%E5%AE%9A"));
    assert_eq!(rich[4]["url"], base);
    assert_eq!(rich[1]["heading"], "Root");
    assert_eq!(rich[3]["heading"], "Sibling Café");
    assert!(rich[2]["body"].contains("## This is code, not a section"));
    assert!(rich[2]["body"].contains("Conditional body stays in the same section."));
    assert!(!rich[2]["body"].contains("Parent body"));
    assert_eq!(term_count(&output, "heading_context", "root")?, 4);
    fixture.build_profile("baseline")?;
    let baseline = records(&fixture.output("baseline"))?;
    for (original, enriched) in baseline.iter().zip(&rich) {
        for field in [
            "id",
            "heading",
            "body",
            "title",
            "source_url",
            "source_path",
        ] {
            assert_eq!(original[field], enriched[field]);
        }
        assert_eq!(original["url"], base);
        assert!(!original.contains_key("heading_path"));
    }
    Ok(())
}

#[test]
fn hierarchy_accepts_adjacent_empty_html_anchors_without_guessing_fragments() -> TestResult {
    let fixture = Fixture::new(&[(
        "docs/onelake/anchors.md",
        r#"# Article
Intro.
<a id = "html-root"></a>

## Anchored
Body.
<A class='anchor' ID='設定'></A>

### Unicode
Body.
<a id="not-adjacent"></a>
Intervening visible paragraph.
## Unanchored
Body.
<!-- <a id="comment-anchor"></a> -->
## Comment
```html
<a id="code-anchor"></a>
```
## Code
Body.
## Inline <a id="inline-anchor"></a>
Body.
<a id="html-before-markdown"></a>
## Markdown {#markdown-id}
Body.
"#,
    )])?;
    let manifest = fixture.build_profile("hierarchy")?;
    assert_eq!(manifest.documents, 8);
    let rich = records(&fixture.output("hierarchy"))?;
    let base = "https://learn.microsoft.com/fabric/onelake/anchors";
    for (heading, fragment) in [
        ("Article", ""),
        ("Anchored", "#html-root"),
        ("Unicode", "#%E8%A8%AD%E5%AE%9A"),
        ("Unanchored", ""),
        ("Comment", ""),
        ("Code", ""),
        ("Inline", "#inline-anchor"),
        ("Markdown", "#markdown-id"),
    ] {
        let record = rich
            .iter()
            .find(|record| record["heading"] == heading)
            .ok_or("missing HTML-anchor fixture heading")?;
        assert_eq!(record["url"], format!("{base}{fragment}"), "{heading}");
    }
    fixture.build_profile("baseline")?;
    let baseline = records(&fixture.output("baseline"))?;
    for (original, enriched) in baseline.iter().zip(&rich) {
        assert_eq!(original["id"], enriched["id"]);
        assert_eq!(original["body"], enriched["body"]);
        assert_eq!(original["heading"], enriched["heading"]);
        assert_eq!(original["url"], base);
    }
    Ok(())
}

#[test]
fn explicit_noindex_is_disclosed_and_excluded_uniformly_across_profiles() -> TestResult {
    let fixture = Fixture::new(&[
        (
            "docs/docfx.json",
            r#"{"build":{
                "content":[{"files":["**/*.md","**/*.yml"]}],
                "globalMetadata":{"ROBOTS":"INDEX, FOLLOW"},
                "fileMetadata":{"ROBOTS":{
                    "onelake/inherited.md":"NOINDEX, NOFOLLOW",
                    "onelake/override.md":"NOINDEX"
                }}
            }}"#,
        ),
        ("docs/onelake/public.md", "# Public\nVisible.\n"),
        (
            "docs/onelake/private.md",
            "---\nROBOTS: NOINDEX\n---\n# Private\nNever searchable.\n",
        ),
        (
            "docs/onelake/inherited.md",
            "# Inherited\nNever searchable.\n",
        ),
        (
            "docs/onelake/override.md",
            "---\nROBOTS: 'INDEX, FOLLOW'\n---\n# Override\nVisible.\n",
        ),
        (
            "docs/onelake/block.md",
            "---\nrObOtS: |\n  NoInDeX\n  FOLLOW\n---\n# Block\nNever searchable.\n",
        ),
        (
            "docs/onelake/list.md",
            "---\nrobots: [follow, noINDEX]\n---\n# List\nNever searchable.\n",
        ),
        (
            "docs/onelake/block-list.md",
            "---\nrobots:\n  - |\n    nOiNdEx\n  - FOLLOW\n---\n# Block list\nNever searchable.\n",
        ),
        (
            "docs/onelake/index.yml",
            "### YamlMime:Landing\ntitle: Hidden landing\nmetadata:\n  robots: 'noindex, follow'\nsummary: Never searchable.\n",
        ),
        (
            "docs/onelake/block.yml",
            "### YamlMime:Landing\ntitle: Hidden block landing\nmetadata:\n  ROBOTS: |\n    NOINDEX\n    FOLLOW\nsummary: Never searchable.\n",
        ),
    ])?;
    for profile in [
        "baseline",
        "stemmed",
        "metadata",
        "hierarchy",
        "autocomplete",
        "full",
    ] {
        let manifest = fixture.build_profile(profile)?;
        assert_eq!(manifest.documents, 2, "{profile}");
        assert_eq!(manifest.source_markdown_files, 7);
        for path in [
            "docs/onelake/private.md",
            "docs/onelake/inherited.md",
            "docs/onelake/block.md",
            "docs/onelake/list.md",
            "docs/onelake/block-list.md",
            "docs/onelake/index.yml",
            "docs/onelake/block.yml",
        ] {
            assert!(
                manifest
                    .skipped_inputs
                    .contains(&format!("{path}: robots NOINDEX metadata")),
                "{profile}: {path}"
            );
            assert!(
                !manifest
                    .filter_catalog
                    .get("source_path")
                    .is_some_and(|paths| paths.contains_key(path))
            );
        }
        let records = records(&fixture.output(profile))?;
        assert_eq!(
            records
                .iter()
                .map(|record| record["heading"].as_str())
                .collect::<Vec<_>>(),
            ["Override", "Public"]
        );
    }
    Ok(())
}

#[test]
fn unsupported_robots_declarations_fail_closed_instead_of_losing_directives() -> TestResult {
    for declaration in [
        "{directives: NOINDEX}",
        "[FOLLOW, {directives: NOINDEX}]",
        "\"NOINDEX\\0\"",
    ] {
        let text = format!("---\nrobots: {declaration}\n---\n# Hidden\nNever searchable.\n");
        let fixture = Fixture::new(&[("docs/onelake/robots.md", &text)])?;
        let error = fixture
            .build("rejected", "none")
            .err()
            .ok_or("silently indexed unsupported robots metadata")?
            .to_string();
        assert!(error.contains("robots metadata"), "{error}");
        assert!(!fixture.output("rejected").join("bundle.json").exists());
    }
    Ok(())
}

#[test]
fn all_profiles_preserve_sections_corpus_codec_and_finalization() -> TestResult {
    let fixture = Fixture::new(&[(
        "docs/onelake/article.md",
        "---\ntitle: UnifiedLake Café\ndescription: Explicit description.\naliases: [Named alias]\nms.topic: tutorial\n---\n# Parent\nRunning connected workloads.\n## Child\nSecond body.\n",
    )])?;
    let profiles = [
        "baseline",
        "stemmed",
        "metadata",
        "hierarchy",
        "autocomplete",
        "full",
    ];
    let mut baseline = None;
    for profile in profiles {
        let expected = IndexFeatures::profile(profile).ok_or("missing profile")?;
        let manifest = fixture.build_profile(profile)?;
        let output = fixture.output(profile);
        let records = records(&output)?;
        assert_eq!(manifest.features, expected);
        assert_eq!(manifest.schema_id, fabric_docs_search::SCHEMA_ID);
        assert_eq!(manifest.documents, 2);
        assert_eq!(manifest.source_markdown_files, 1);
        assert_eq!(manifest.source_commit, fixture.revision);
        assert_eq!(manifest.compression, "none");
        assert_eq!(manifest.segments, 1);
        assert!(manifest.unresolved_includes.is_empty());
        assert!(manifest.skipped_inputs.is_empty());
        assert_eq!(!manifest.filter_catalog.is_empty(), expected.metadata);
        for record in &records {
            for field in ["metadata_json", "description", "aliases"] {
                assert_eq!(record.contains_key(field), expected.metadata, "{profile}");
            }
            assert_eq!(record.contains_key("heading_path"), expected.hierarchy);
        }
        for (field, term, enabled) in [
            ("body_stem", "run", expected.stemmed),
            ("metadata_terms", "ms.topic=tutorial", expected.metadata),
            ("heading_context", "parent", expected.hierarchy),
            ("completion", "unif", expected.autocomplete),
        ] {
            assert_eq!(term_count(&output, field, term)? > 0, enabled, "{profile}");
        }
        let legacy = legacy_records(records);
        if let Some(baseline) = &baseline {
            assert_eq!(&legacy, baseline, "{profile}");
        } else {
            baseline = Some(legacy);
        }
        let persisted: BundleManifest =
            serde_json::from_slice(&fs::read(output.join("bundle.json"))?)?;
        assert_eq!(persisted.features, expected);
        assert_eq!(persisted.filter_catalog, manifest.filter_catalog);
    }
    assert!(IndexFeatures::profile("Full").is_none());
    assert!(IndexFeatures::profile("semantic").is_none());
    let default = fixture.build("default", "none")?;
    assert_eq!(default.features, IndexFeatures::full());
    Ok(())
}

#[test]
fn completions_are_bounded_unicode_word_edges_without_stemming() -> TestResult {
    let long_word = "supercalifragilisticexpialidocious";
    let prefix20 = long_word.chars().take(20).collect::<String>();
    let prefix21 = long_word.chars().take(21).collect::<String>();
    let source = format!(
        "---\ntitle: UnifiedLake Café ÉCOLE 東京設定 {long_word}\n---\n# Separate Word\nRunning appears only in body.\n"
    );
    let fixture = Fixture::new(&[("docs/onelake/completion.md", &source)])?;
    fixture.build_profile("autocomplete")?;
    let output = fixture.output("autocomplete");
    for term in [
        "un",
        "unif",
        "unifiedlake",
        "ca",
        "caf",
        "café",
        "éc",
        "école",
        "東京",
        "東京設",
        "東京設定",
        "se",
        "separate",
        "wo",
        "word",
        &prefix20,
    ] {
        assert_eq!(term_count(&output, "completion", term)?, 1, "{term}");
    }
    for term in [
        "u",
        "c",
        "東",
        "UnifiedLake",
        "ÉC",
        "separateword",
        "running",
        "run",
        long_word,
        &prefix21,
    ] {
        assert_eq!(term_count(&output, "completion", term)?, 0, "{term}");
    }
    Ok(())
}

#[test]
fn builder_binary_reports_json_and_nonzero_failures() -> TestResult {
    let fixture = Fixture::new(&[("docs/onelake/article.md", "# Article\nCLI fixture text.\n")])?;
    let binary = env!("CARGO_BIN_EXE_build-docs-index");
    let output = Command::new(binary)
        .arg("--source")
        .arg(&fixture.source)
        .arg("--output")
        .arg(fixture.output("binary"))
        .arg("--revision")
        .arg(&fixture.revision)
        .args(["--compression", "none"])
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let manifest: BundleManifest = serde_json::from_slice(&output.stdout)?;
    assert_eq!(manifest.documents, 1);
    assert_eq!(manifest.source_commit, fixture.revision);
    assert_eq!(manifest.features, IndexFeatures::full());
    let baseline = Command::new(binary)
        .arg("--source")
        .arg(&fixture.source)
        .arg("--output")
        .arg(fixture.output("baseline-binary"))
        .arg("--revision")
        .arg(&fixture.revision)
        .args(["--compression", "none", "--profile", "baseline"])
        .output()?;
    assert!(
        baseline.status.success(),
        "{}",
        String::from_utf8_lossy(&baseline.stderr)
    );
    let baseline: BundleManifest = serde_json::from_slice(&baseline.stdout)?;
    assert_eq!(baseline.features, IndexFeatures::default());
    assert!(baseline.filter_catalog.is_empty());
    let invalid_profile = Command::new(binary)
        .arg("--source")
        .arg(&fixture.source)
        .arg("--output")
        .arg(fixture.output("invalid-profile"))
        .arg("--revision")
        .arg(&fixture.revision)
        .args(["--profile", "FULL"])
        .output()?;
    assert!(!invalid_profile.status.success());
    assert!(invalid_profile.stdout.is_empty());
    assert!(!fixture.output("invalid-profile").exists());
    let failure = Command::new(binary)
        .arg("--source")
        .arg(&fixture.source)
        .arg("--output")
        .arg(fixture.output("bad-binary"))
        .args(["--revision", "main", "--compression", "none"])
        .output()?;
    assert!(!failure.status.success());
    assert!(failure.stdout.is_empty());
    assert!(!failure.stderr.is_empty());
    assert!(!fixture.output("bad-binary").join("bundle.json").exists());
    Ok(())
}
