// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Offline, revision-pinned Fabric article ingestion and finalized bundle creation.
//! Source bytes come from Git objects, never mutable working-tree article files.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    error::Error,
    fs,
    io::{self, BufRead, BufReader, Read, Write},
    ops::Range,
    path::{Component as PathComponent, Path},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
};

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use ring::digest::{Context, SHA256};
use serde::Deserialize;
use tantivy::{
    Index, IndexSettings, IndexWriter, ReloadPolicy, TantivyDocument, merge_policy::NoMergePolicy,
    store::Compressor,
};
use yaml_rust2::{Yaml, YamlLoader};

use crate::{
    BundleManifest, Component, LicenseNotice, SCHEMA_ID, index_schema, register_tokenizers,
    rich::{IndexFeatures, Metadata},
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

const REPOSITORY: &str = "https://github.com/MicrosoftDocs/fabric-docs";
const MAX_SOURCE_BYTES: usize = 32 * 1024 * 1024;
const MAX_INCLUDE_DEPTH: usize = 32;
const MAX_METADATA_VALUES: usize = 256;
const REVIEWED_ITEM_TYPE_ARTICLES: usize = 10;
const WRITER_MEMORY_BYTES: usize = 50_000_000;
const INCLUDE_DIRECTIVE: &str = "[!INCLUDE";
const DEPENDENCIES: &[&str] = &[
    "powerbi-repo",
    "kusto-repo",
    "powerquery-repo",
    "reusable-content",
];

/// Build a new offline index from a local MicrosoftDocs/fabric-docs checkout.
///
/// Both paths must be absolute; `output` must not exist and must be outside
/// `source`. Its parent must already exist. `revision` must be a full 40-hex
/// commit matching checkout HEAD. Only committed objects at that revision are
/// read, with Git lazy fetching disabled. No network or publishing dependencies
/// are fetched. Git 2.36 or newer is required for its batch-command reader.
/// `compression` is exactly `lz4`, `zstd`, or `none`. Individual inputs and
/// expanded articles are limited to 32 MiB, and includes to 32 nested files.
/// All lexical additions are enabled; use [`build_with_features`] for ablations.
///
/// The returned manifest is also written to `output/bundle.json`, after merging,
/// reopening, and verifying the index. Record IDs and inventories are stable,
/// but index bytes are not deterministic: Tantivy generates segment UUIDs.
///
/// # Errors
///
/// Returns input, Git, UTF-8/YAML/DocFX, include safety, filesystem, or Tantivy
/// errors without substituting content. Failed builds can leave an incomplete
/// output directory; only a successfully written `bundle.json` is a completion
/// marker.
pub fn build(
    source: &Path,
    output: &Path,
    revision: &str,
    compression: &str,
) -> Result<BundleManifest> {
    build_with_features(source, output, revision, compression, IndexFeatures::full())
}

/// Build a pinned offline bundle with independently selected lexical additions.
///
/// The source, output, revision, compression, safety, and finalization contracts
/// are identical to [`build`]. Features only add postings/stored fields; article
/// selection, section ordinals, codecs, and segment finalization are unchanged.
/// Metadata uses committed DocFX global/file metadata, overridden by the source
/// article. All distinct matching file-metadata values are retained as configured
/// values, without inferring which one a publishing tool would choose; global
/// values are used only when no file rule matches that key. Only bounded public
/// scalar values and scalar arrays are retained.
/// Decoded metadata strings remain exact, including commas and whitespace;
/// splitting and trimming apply only to the derived lexical aliases field.
/// Articles exceeding 256 retained metadata values, including structural facts,
/// fail rather than publishing an incomplete filter vocabulary.
/// Explicit `robots` `NOINDEX` directives exclude articles in every profile and
/// are checked before display-value filtering and disclosed in skipped inputs.
/// Heading paths contain actual source headings, without a synthetic title root;
/// only explicit Markdown IDs or adjacent empty HTML anchors produce fragment links.
/// Completion postings contain deduplicated lowercase word-edge prefixes of
/// 2 through 20 Unicode characters from each source title and heading.
/// The built-in reviewed item-type map applies only to its exact source revision
/// with metadata enabled. Its exact paths, classifications, and evidence are
/// disclosed in transformation attribution; unmapped articles remain unknown.
///
/// # Errors
///
/// Returns the same input, source parsing, safety, filesystem, and index errors
/// as [`build`].
pub fn build_with_features(
    source: &Path,
    output: &Path,
    revision: &str,
    compression: &str,
    features: IndexFeatures,
) -> Result<BundleManifest> {
    let codec = match compression {
        "lz4" => Compressor::Lz4,
        "zstd" => Compressor::Zstd(Default::default()),
        "none" => Compressor::None,
        _ => return Err(invalid("compression must be lz4, zstd, or none")),
    };
    if !source.is_absolute() || !output.is_absolute() {
        return Err(invalid("source and output must be absolute paths"));
    }
    if revision.len() != 40 || !revision.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(invalid("revision must be a full 40-hex Git commit"));
    }
    if output
        .components()
        .any(|part| matches!(part, PathComponent::ParentDir))
    {
        return Err(invalid("output must not contain parent traversal"));
    }
    match fs::symlink_metadata(output) {
        Ok(_) => return Err(invalid("output must be a new directory")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let source = source.canonicalize()?;
    let docs = source.join("docs");
    if fs::symlink_metadata(&docs)?.is_symlink() || !docs.is_dir() {
        return Err(invalid("source/docs must be a real directory"));
    }
    let parent = output
        .parent()
        .ok_or_else(|| invalid("output has no parent"))?
        .canonicalize()?;
    let name = output
        .file_name()
        .ok_or_else(|| invalid("output needs a directory name"))?;
    let output = parent.join(name);
    if output.starts_with(&source) {
        return Err(invalid("output must be outside the source checkout"));
    }
    let root = String::from_utf8(git(&source, &["rev-parse", "--show-toplevel"])?)?;
    if Path::new(root.trim()).canonicalize()? != source {
        return Err(invalid("source must be the checkout root"));
    }
    let head = String::from_utf8(git(&source, &["rev-parse", "--verify", "HEAD^{commit}"])?)?;
    if !head.trim().eq_ignore_ascii_case(revision) {
        return Err(invalid("source HEAD does not match the pinned revision"));
    }
    let revision = revision.to_ascii_lowercase();
    let mut snapshot = Snapshot::open(&source, &revision)?;
    let result = build_snapshot(
        &mut snapshot,
        &output,
        &revision,
        compression,
        codec,
        features,
    );
    let closed = snapshot.close();
    let manifest = result?;
    closed?;
    // A HEAD change does not change the blobs read, but invalidates the caller's pin.
    let head = String::from_utf8(git(&source, &["rev-parse", "--verify", "HEAD^{commit}"])?)?;
    if head.trim() != revision {
        return Err(invalid("source HEAD changed during indexing"));
    }
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output.join("bundle.json"))?;
    serde_json::to_writer_pretty(&mut file, &manifest)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(manifest)
}

fn build_snapshot(
    snapshot: &mut Snapshot,
    output: &Path,
    revision: &str,
    compression: &str,
    codec: Compressor,
    features: IndexFeatures,
) -> Result<BundleManifest> {
    let docfx = snapshot.read_text("docs/docfx.json")?;
    let rules = publication_rules(&docfx)?;
    let inherited = DocfxMetadata::parse(&docfx)?;
    let item_types = reviewed_item_types(revision, features)?;
    if let Some(item_types) = &item_types {
        item_types.validate_paths(&snapshot.entries)?;
    }
    let mut manifest = BundleManifest {
        features,
        filter_catalog: BTreeMap::new(),
        schema_version: 1,
        tantivy_version: "0.27.0".to_owned(),
        schema_id: SCHEMA_ID.to_owned(),
        source_repository: REPOSITORY.to_owned(),
        source_commit: revision.to_owned(),
        source_markdown_files: 0,
        source_markdown_bytes: 0,
        documents: 0,
        compression: compression.to_owned(),
        segments: 0,
        unresolved_includes: Vec::new(),
        skipped_inputs: Vec::new(),
        licenses: Vec::new(),
        files: Vec::new(),
    };
    for name in ["LICENSE", "LICENSE-CODE", "ThirdPartyNotices.md"] {
        let path = if snapshot.entries.contains_key(name) {
            name.to_owned()
        } else {
            format!("docs/{name}")
        };
        manifest.licenses.push(LicenseNotice {
            name: name.to_owned(),
            text: snapshot.read_text(&path)?,
        });
    }
    let mut attribution = format!(
        "Derived from {REPOSITORY} at commit {revision}. Microsoft Fabric CLI \
             transforms committed Markdown and clearly identified YAML landing content \
             into plain-text heading sections, resolves local includes, excludes images, \
             comments, and articles with explicit robots NOINDEX metadata, and optionally \
             indexes bounded source-authored metadata, heading context, stemmed prose, \
             and completion terms with Tantivy 0.27.0. \
             Matching configured DocFX file metadata values are retained without inferring \
             a publishing winner; explicit article metadata overrides each key. Original source \
             notices above remain applicable. External Power BI, Kusto, Power Query, \
             and reusable-content publishing dependencies are not included; unavailable \
             include locations are recorded in unresolved_includes. This is not a \
             complete snapshot of Microsoft Learn or those external docsets."
    );
    if let Some(item_types) = &item_types {
        attribution.push_str(&item_types.attribution());
    }
    manifest.licenses.push(LicenseNotice {
        name: "TRANSFORMATION-ATTRIBUTION".to_owned(),
        text: attribution,
    });
    let markdown_paths: Vec<_> = snapshot
        .entries
        .keys()
        .filter(|path| path.starts_with("docs/") && extension(path) == "md")
        .cloned()
        .collect();
    manifest.source_markdown_files = markdown_paths.len();
    for path in markdown_paths {
        manifest.source_markdown_bytes += snapshot.blob_size(&path)?;
    }
    let mut candidates = Vec::new();
    let mut skipped = BTreeSet::new();
    for (path, entry) in &snapshot.entries {
        let Some(relative) = path.strip_prefix("docs/") else {
            continue;
        };
        let extension = extension(path);
        if !matches!(extension.as_str(), "md" | "yml" | "yaml") {
            continue;
        }
        let reason = if standalone_excluded(relative) {
            Some("non-article or include fragment")
        } else if !rules.iter().any(|rule| rule.publishes(relative)) {
            Some("DocFX source exclusion")
        } else {
            None
        };
        if let Some(reason) = reason {
            skipped.insert(format!("{path}: {reason}"));
        } else {
            if entry.mode != "100644" && entry.mode != "100755" {
                return Err(invalid(format!("non-regular article input: {path}")));
            }
            candidates.push(path.clone());
        }
    }
    let mut includes = Includes::default();
    // Discover even fragments outside includes/ before choosing standalone articles.
    for path in &candidates {
        if extension(path) == "md" {
            includes.expand(snapshot, path, &mut Vec::new())?;
        }
    }
    fs::create_dir(output)?;
    let index = Index::create(
        tantivy::directory::MmapDirectory::open(output)?,
        index_schema(),
        IndexSettings {
            docstore_compression: codec,
            ..Default::default()
        },
    )?;
    register_tokenizers(&index)?;
    let mut writer: IndexWriter<TantivyDocument> =
        index.writer_with_num_threads(1, WRITER_MEMORY_BYTES)?;
    writer.set_merge_policy(Box::new(NoMergePolicy));
    let mut classified_articles = BTreeSet::new();
    for path in candidates {
        if includes.fragments.contains(&path) {
            skipped.insert(format!("{path}: referenced include fragment"));
            continue;
        }
        let relative = path
            .strip_prefix("docs/")
            .ok_or_else(|| invalid("article outside docs"))?;
        let mut metadata = inherited.for_path(relative);
        let article = if extension(&path) == "md" {
            let raw = snapshot.read_text(&path)?;
            let (title, authored, _) = frontmatter(&raw)?;
            metadata.extend(authored);
            let expanded = includes.expand(snapshot, &path, &mut Vec::new())?;
            Some(markdown_article(title, &expanded))
        } else {
            landing_article(&snapshot.read_text(&path)?, &mut metadata)?
        };
        let Some(article) = article else {
            skipped.insert(format!("{path}: not a YAML landing page"));
            continue;
        };
        if noindex(&metadata) {
            skipped.insert(format!("{path}: robots NOINDEX metadata"));
            continue;
        }
        if article.sections.is_empty() {
            skipped.insert(format!("{path}: empty article"));
            continue;
        }
        let workload = relative
            .split('/')
            .next()
            .filter(|_| relative.contains('/'))
            .unwrap_or("fabric");
        if let Some(item_types) = &item_types
            && item_types.apply(&path, &mut metadata)?
        {
            classified_articles.insert(path.clone());
        }
        let metadata = bounded_metadata(metadata, workload, &path)?;
        if features.metadata {
            for (field, values) in &metadata {
                for value in values {
                    *manifest
                        .filter_catalog
                        .entry(field.clone())
                        .or_default()
                        .entry(value.clone())
                        .or_default() += 1;
                }
            }
        }
        let description = metadata_string(&metadata, "description")
            .map(|value| plain_text(&value))
            .unwrap_or_default();
        let aliases = ["aliases", "ms.search.form", "ms.search"]
            .into_iter()
            .filter_map(|field| metadata.get(field))
            .flatten()
            .flat_map(|value| value.split(','))
            .map(|value| plain_text(value.trim()))
            .filter(|value| !value.is_empty())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
            .join("\n");
        for section in article.sections {
            let identifiers = format!("{}\n{}\n{}", article.title, section.heading, section.body);
            let mut document = TantivyDocument::default();
            if features.stemmed {
                document.add_text(index.schema().get_field("body_stem")?, &section.body);
            }
            if features.metadata {
                if !description.is_empty() {
                    document.add_text(index.schema().get_field("description")?, &description);
                }
                if !aliases.is_empty() {
                    document.add_text(index.schema().get_field("aliases")?, &aliases);
                }
                document.add_text(
                    index.schema().get_field("metadata_json")?,
                    serde_json::to_string(&metadata)?,
                );
                for (field, values) in &metadata {
                    for value in values {
                        document.add_text(
                            index.schema().get_field("metadata_terms")?,
                            format!("{field}={value}"),
                        );
                    }
                }
            }
            let mut url = article_url(relative);
            if features.hierarchy {
                document.add_text(
                    index.schema().get_field("heading_path")?,
                    serde_json::to_string(&section.heading_path)?,
                );
                document.add_text(
                    index.schema().get_field("heading_context")?,
                    section.heading_path.join("\n"),
                );
                if let Some(anchor) = &section.anchor {
                    url.push('#');
                    url.push_str(&url_path(anchor));
                }
            }
            if features.autocomplete {
                document.add_text(
                    index.schema().get_field("completion")?,
                    completion_terms(&article.title, &section.heading),
                );
            }
            for (field, text) in [
                ("id", format!("{path}#section-{}", section.ordinal)),
                ("title", article.title.clone()),
                ("heading", section.heading),
                ("body", section.body),
                ("identifiers", identifiers),
                ("workload", workload.to_owned()),
                ("url", url),
                (
                    "source_url",
                    format!("{REPOSITORY}/blob/{revision}/{}", url_path(&path)),
                ),
                ("source_path", path.clone()),
            ] {
                document.add_text(index.schema().get_field(field)?, text);
            }
            writer.add_document(document)?;
            manifest.documents += 1;
        }
    }
    if manifest.documents == 0 {
        return Err(invalid(
            "the pinned source contains no published article sections",
        ));
    }
    if let Some(item_types) = &item_types
        && classified_articles.len() != item_types.articles.len()
    {
        return Err(invalid(
            "reviewed item-type map includes an article without indexed sections",
        ));
    }
    writer.commit()?;
    let segments = index.searchable_segment_ids()?;
    writer.merge(&segments).wait()?;
    writer.garbage_collect_files().wait()?;
    writer.wait_merging_threads()?;
    let finalized = index.load_metas()?;
    if finalized.segments.len() != 1 {
        return Err(invalid("the finalized index must have exactly one segment"));
    }
    let mut retained = BTreeSet::from(["meta.json".to_owned()]);
    for segment in &finalized.segments {
        for component in [
            tantivy::index::SegmentComponent::Postings,
            tantivy::index::SegmentComponent::Positions,
            tantivy::index::SegmentComponent::Terms,
            tantivy::index::SegmentComponent::Store,
            tantivy::index::SegmentComponent::FastFields,
            tantivy::index::SegmentComponent::FieldNorms,
        ] {
            let name = segment
                .relative_path(component)
                .into_os_string()
                .into_string()
                .map_err(|_| invalid("non-UTF-8 segment component name"))?;
            retained.insert(name);
        }
    }
    manifest.segments = finalized.segments.len();
    drop(finalized);
    drop(index);
    let reopened = Index::open_in_dir(output)?;
    register_tokenizers(&reopened)?;
    let reader: tantivy::IndexReader = reopened
        .reader_builder()
        .reload_policy(ReloadPolicy::Manual)
        .try_into()?;
    if reader.searcher().num_docs() != manifest.documents
        || reopened.settings().docstore_compression != codec
    {
        return Err(invalid(
            "reopened index count or compression does not match the build",
        ));
    }
    drop(reader);
    drop(reopened);
    // Export only committed reader components, not locks or writer bookkeeping.
    // All handles are gone and this directory was created by this call.
    for entry in fs::read_dir(output)? {
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| invalid("non-UTF-8 component name"))?;
        if !retained.contains(&name) {
            fs::remove_file(entry.path())?;
        }
    }
    for name in retained {
        let path = output.join(&name);
        let mut file = fs::File::open(path)?;
        let bytes = file.metadata()?.len();
        let mut checksum = Context::new(&SHA256);
        let mut buffer = [0; 64 * 1024];
        loop {
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            checksum.update(&buffer[..count]);
        }
        let sha256 = checksum
            .finish()
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        manifest.files.push(Component {
            name,
            bytes,
            sha256,
        });
    }
    manifest.unresolved_includes = includes.unresolved.into_iter().collect();
    manifest.skipped_inputs = skipped.into_iter().collect();
    Ok(manifest)
}

fn invalid(message: impl Into<String>) -> Box<dyn Error> {
    Box::new(io::Error::new(io::ErrorKind::InvalidData, message.into()))
}

fn git_command(source: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .args([
            "--no-pager",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.longpaths=true",
            "-C",
        ])
        .arg(source);
    command
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .env("GIT_TERMINAL_PROMPT", "0");
    for name in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
    ] {
        command.env_remove(name);
    }
    command
}

fn git(source: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let output = git_command(source)
        .args(args)
        .stdin(Stdio::null())
        .output()?;
    if !output.status.success() {
        return Err(invalid(format!(
            "local Git {} failed: {}",
            args[0],
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(output.stdout)
}

struct Entry {
    mode: String,
    object: String,
}

struct Snapshot {
    entries: BTreeMap<String, Entry>,
    process: Child,
    input: Option<ChildStdin>,
    output: BufReader<ChildStdout>,
}

impl Snapshot {
    fn open(source: &Path, revision: &str) -> Result<Self> {
        let tree = git(source, &["ls-tree", "-r", "-z", "--full-tree", revision])?;
        let mut entries = BTreeMap::new();
        for entry in tree
            .split(|byte| *byte == 0)
            .filter(|entry| !entry.is_empty())
        {
            let line = std::str::from_utf8(entry)?;
            let (header, path) = line
                .split_once('\t')
                .ok_or_else(|| invalid("invalid Git tree entry"))?;
            let fields: Vec<_> = header.split_whitespace().collect();
            if fields.len() != 3 {
                return Err(invalid("invalid Git tree metadata"));
            }
            validate_repo_path(path)?;
            entries.insert(
                path.to_owned(),
                Entry {
                    mode: fields[0].to_owned(),
                    object: fields[2].to_owned(),
                },
            );
        }
        let mut process = git_command(source)
            .args(["cat-file", "--batch-command"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        let input = process
            .stdin
            .take()
            .ok_or_else(|| invalid("Git blob input unavailable"))?;
        let output = process
            .stdout
            .take()
            .ok_or_else(|| invalid("Git blob output unavailable"))?;
        Ok(Self {
            entries,
            process,
            input: Some(input),
            output: BufReader::new(output),
        })
    }

    fn request(&mut self, path: &str, operation: &str) -> Result<u64> {
        validate_repo_path(path)?;
        let entry = self
            .entries
            .get(path)
            .ok_or_else(|| invalid(format!("missing committed input: {path}")))?;
        if !matches!(entry.mode.as_str(), "100644" | "100755") {
            return Err(invalid(format!("symlink or non-regular input: {path}")));
        }
        let input = self
            .input
            .as_mut()
            .ok_or_else(|| invalid("Git blob input is closed"))?;
        writeln!(input, "{operation} {}", entry.object)?;
        input.flush()?;
        let mut header = String::new();
        self.output.read_line(&mut header)?;
        let fields: Vec<_> = header.split_whitespace().collect();
        if fields.len() != 3 || fields[0] != entry.object || fields[1] != "blob" {
            return Err(invalid(format!(
                "committed blob is unavailable or inconsistent: {path}"
            )));
        }
        Ok(fields[2].parse()?)
    }

    fn blob_size(&mut self, path: &str) -> Result<u64> {
        self.request(path, "info")
    }

    fn read_text(&mut self, path: &str) -> Result<String> {
        let size = self.blob_size(path)?;
        if size > MAX_SOURCE_BYTES as u64 {
            return Err(invalid(format!("source input exceeds 32 MiB: {path}")));
        }
        if self.request(path, "contents")? != size {
            return Err(invalid("Git blob size changed between requests"));
        }
        let mut bytes = vec![0; usize::try_from(size)?];
        self.output.read_exact(&mut bytes)?;
        let mut newline = [0];
        self.output.read_exact(&mut newline)?;
        if newline != *b"\n" {
            return Err(invalid("invalid Git blob terminator"));
        }
        String::from_utf8(bytes).map_err(Into::into)
    }

    fn close(&mut self) -> Result<()> {
        self.input.take();
        if !self.process.wait()?.success() {
            return Err(invalid("local Git blob reader failed"));
        }
        Ok(())
    }
}

impl Drop for Snapshot {
    fn drop(&mut self) {
        self.input.take();
        // On an ingestion error, still reap the local read-only Git child.
        let _ = self.process.wait();
    }
}

fn validate_repo_path(path: &str) -> Result<()> {
    if path.is_empty()
        || path.contains(['\\', ':', '\0', '\n', '\r'])
        || path
            .split('/')
            .any(|part| part.is_empty() || matches!(part, "." | ".."))
    {
        return Err(invalid("unsafe repository-relative source path"));
    }
    Ok(())
}

fn extension(path: &str) -> String {
    path.rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .unwrap_or_default()
}

fn standalone_excluded(path: &str) -> bool {
    path.split('/').any(|part| {
        matches!(
            part.to_ascii_lowercase().as_str(),
            "includes" | "include" | "obj" | ".github" | "_themes" | "_themes.pdf" | "_repo.en-us"
        )
    }) || matches!(
        path.rsplit('/')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str(),
        "toc.md"
            | "toc.yml"
            | "toc.yaml"
            | "readme.md"
            | "license"
            | "license-code"
            | "thirdpartynotices.md"
    )
}

struct ContentRule {
    prefix: String,
    files: Vec<String>,
    exclude: Vec<String>,
}

impl ContentRule {
    fn publishes(&self, path: &str) -> bool {
        let Some(path) = path.strip_prefix(&self.prefix) else {
            return false;
        };
        self.files.iter().any(|pattern| glob_matches(pattern, path))
            && !self
                .exclude
                .iter()
                .any(|pattern| glob_matches(pattern, path))
    }
}

fn publication_rules(text: &str) -> Result<Vec<ContentRule>> {
    let config: serde_json::Value = serde_json::from_str(text)?;
    let content = config["build"]["content"]
        .as_array()
        .ok_or_else(|| invalid("DocFX build.content must be an array"))?;
    let mut rules = Vec::new();
    for item in content {
        let source = item["src"].as_str().unwrap_or(".");
        let source = source.trim_start_matches("./").trim_end_matches('/');
        let source = if source == "." { "" } else { source };
        if !source.is_empty() {
            validate_repo_path(source)?;
        }
        let patterns = |name: &str| -> Result<Vec<String>> {
            match item.get(name) {
                None if name == "exclude" => Ok(Vec::new()),
                Some(serde_json::Value::String(value)) => {
                    Ok(vec![value.trim_start_matches("./").to_owned()])
                }
                Some(serde_json::Value::Array(values)) => values
                    .iter()
                    .map(|value| {
                        value
                            .as_str()
                            .map(|value| value.trim_start_matches("./").to_owned())
                            .ok_or_else(|| invalid("DocFX glob must be a string"))
                    })
                    .collect(),
                _ => Err(invalid(format!("DocFX {name} must be a string or array"))),
            }
        };
        rules.push(ContentRule {
            prefix: if source.is_empty() {
                String::new()
            } else {
                format!("{source}/")
            },
            files: patterns("files")?,
            exclude: patterns("exclude")?,
        });
    }
    Ok(rules)
}

fn glob_matches(pattern: &str, path: &str) -> bool {
    fn matches(pattern: &[u8], path: &[u8], memo: &mut HashMap<(usize, usize), bool>) -> bool {
        let key = (pattern.len(), path.len());
        if let Some(result) = memo.get(&key) {
            return *result;
        }
        let result = if pattern.is_empty() {
            path.is_empty()
        } else if pattern.starts_with(b"**/") {
            matches(&pattern[3..], path, memo)
                || path.iter().enumerate().any(|(index, byte)| {
                    *byte == b'/' && matches(&pattern[3..], &path[index + 1..], memo)
                })
        } else if pattern[0] == b'*' {
            let double = pattern.get(1) == Some(&b'*');
            matches(&pattern[if double { 2 } else { 1 }..], path, memo)
                || !path.is_empty()
                    && (double || path[0] != b'/')
                    && matches(pattern, &path[1..], memo)
        } else {
            !path.is_empty()
                && (pattern[0] == path[0] || pattern[0] == b'?' && path[0] != b'/')
                && matches(&pattern[1..], &path[1..], memo)
        };
        memo.insert(key, result);
        result
    }
    matches(pattern.as_bytes(), path.as_bytes(), &mut HashMap::new())
}

#[derive(Deserialize)]
struct ItemTypeMap {
    schema_version: u32,
    source_commit: String,
    articles: BTreeMap<String, ItemTypeClassification>,
}

#[derive(Deserialize)]
struct ItemTypeClassification {
    item_types: Vec<String>,
    evidence: String,
}

fn reviewed_item_types(revision: &str, features: IndexFeatures) -> Result<Option<ItemTypeMap>> {
    if !features.metadata {
        return Ok(None);
    }
    let map = ItemTypeMap::parse(include_str!("../data/item-types.json"))?;
    Ok((revision == map.source_commit).then_some(map))
}

impl ItemTypeMap {
    fn parse(text: &str) -> Result<Self> {
        let map: Self = serde_json::from_str(text)?;
        if map.schema_version != 1
            || map.source_commit.len() != 40
            || !map
                .source_commit
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            || map.articles.len() != REVIEWED_ITEM_TYPE_ARTICLES
        {
            return Err(invalid(
                "invalid reviewed item-type map schema, pin, or article count",
            ));
        }
        for (path, classification) in &map.articles {
            validate_repo_path(path)?;
            if !path.starts_with("docs/")
                || !matches!(extension(path).as_str(), "md" | "yml" | "yaml")
                || classification.item_types.is_empty()
                || classification.item_types.len() > MAX_METADATA_VALUES
                || classification.item_types.iter().any(|value| {
                    metadata_value(value.clone()).is_none()
                        || value.trim() != value
                        || !value
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
                })
                || classification
                    .item_types
                    .iter()
                    .collect::<BTreeSet<_>>()
                    .len()
                    != classification.item_types.len()
                || metadata_value(classification.evidence.clone()).is_none()
            {
                return Err(invalid(format!(
                    "invalid reviewed item-type classification: {path}"
                )));
            }
        }
        Ok(map)
    }

    fn validate_paths(&self, entries: &BTreeMap<String, Entry>) -> Result<()> {
        for path in self.articles.keys() {
            if !entries
                .get(path)
                .is_some_and(|entry| matches!(entry.mode.as_str(), "100644" | "100755"))
            {
                return Err(invalid(format!(
                    "reviewed item-type article is not a committed regular input: {path}"
                )));
            }
        }
        Ok(())
    }

    fn apply(&self, path: &str, metadata: &mut Metadata) -> Result<bool> {
        let Some(classification) = self.articles.get(path) else {
            return Ok(false);
        };
        let expected = classification.item_types.iter().collect::<BTreeSet<_>>();
        for field in ["item_type", "itemType", "ms.item_type"] {
            if let Some(existing) = metadata.get(field)
                && existing.iter().collect::<BTreeSet<_>>() != expected
            {
                return Err(invalid(format!(
                    "source item-type metadata conflicts with the reviewed exact-article map: {path}"
                )));
            }
        }
        metadata
            .entry("item_type".to_owned())
            .or_insert_with(|| classification.item_types.clone());
        Ok(true)
    }

    fn attribution(&self) -> String {
        let mut notice = format!(
            "\nReviewed item-type supplement: fabric-cli \
             rust/engine/control-plane/search/docs/data/item-types.json, schema {}. \
             This exact-article map is bound to {REPOSITORY} at commit {} and is \
             applied only with metadata enabled. Its item_type values are reviewed \
             classifications, not original article frontmatter. No path-prefix, \
             prose-mention, navigation-card, routing-label, or code-example inference \
             is used. Unmapped articles remain unclassified. Evidence:\n",
            self.schema_version, self.source_commit,
        );
        for (path, classification) in &self.articles {
            notice.push_str(&format!(
                "- {path}: {}. {} Source: {REPOSITORY}/blob/{}/{}\n",
                classification.item_types.join(", "),
                classification.evidence,
                self.source_commit,
                url_path(path),
            ));
        }
        notice
    }
}

#[derive(Default)]
struct DocfxMetadata {
    global: Metadata,
    files: BTreeMap<String, Vec<(String, Vec<String>)>>,
}

impl DocfxMetadata {
    fn parse(text: &str) -> Result<Self> {
        let config: serde_json::Value = serde_json::from_str(text)?;
        let mut metadata = Self::default();
        if let Some(global) = config["build"]["globalMetadata"].as_object() {
            for (field, value) in global {
                if metadata_field(field) {
                    metadata
                        .global
                        .insert(field.clone(), json_values(field, value)?);
                }
            }
        }
        if let Some(files) = config["build"]["fileMetadata"].as_object() {
            for (field, patterns) in files {
                if !metadata_field(field) {
                    continue;
                }
                if let Some(patterns) = patterns.as_object() {
                    for (pattern, value) in patterns {
                        metadata.files.entry(field.clone()).or_default().push((
                            pattern.trim_start_matches("./").to_owned(),
                            json_values(field, value)?,
                        ));
                    }
                }
            }
        }
        Ok(metadata)
    }

    fn for_path(&self, path: &str) -> Metadata {
        let mut metadata = self.global.clone();
        for (field, patterns) in &self.files {
            let matching = patterns
                .iter()
                .filter(|(pattern, _)| glob_matches(pattern, path))
                .collect::<Vec<_>>();
            if !matching.is_empty() {
                let mut seen = BTreeSet::new();
                let values = matching
                    .into_iter()
                    .flat_map(|(_, values)| values.iter().cloned())
                    .filter(|value| seen.insert(value.clone()))
                    .collect();
                metadata.insert(field.clone(), values);
            }
        }
        metadata
    }
}

fn metadata_field(field: &str) -> bool {
    !field.is_empty()
        && field.len() <= 80
        && !field.starts_with(['_', '.'])
        && field
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
        && !matches!(
            field.to_ascii_lowercase().replace(['_', '-'], "").as_str(),
            "image"
                | "images"
                | "imagesrc"
                | "imageurl"
                | "thumbnail"
                | "thumbnailurl"
                | "alttext"
                | "icon"
                | "iconsrc"
                | "ms.socialimageurl"
        )
}

fn metadata_value(value: String) -> Option<String> {
    (!value.trim().is_empty() && value.len() <= 512 && !value.chars().any(char::is_control))
        .then_some(value)
}

fn metadata_values(field: &str, values: Option<Vec<String>>) -> Result<Vec<String>> {
    let robots = field.eq_ignore_ascii_case("robots");
    let Some(values) = values else {
        return if robots {
            Err(invalid("robots metadata must be a scalar or scalar array"))
        } else {
            Ok(Vec::new())
        };
    };
    if robots
        && values.iter().any(|value| {
            value
                .chars()
                .any(|character| character.is_control() && !character.is_ascii_whitespace())
        })
    {
        return Err(invalid(
            "robots metadata contains unsupported control characters",
        ));
    }
    let mut seen = BTreeSet::new();
    // Keep directive evidence until NOINDEX evaluation, before display-value filtering.
    Ok(values
        .into_iter()
        .filter(|value| robots || metadata_value(value.clone()).is_some())
        .filter(|value| seen.insert(value.clone()))
        .collect())
}

fn json_values(field: &str, value: &serde_json::Value) -> Result<Vec<String>> {
    fn scalar(value: &serde_json::Value) -> Option<String> {
        match value {
            serde_json::Value::String(value) => Some(value.clone()),
            serde_json::Value::Bool(value) => Some(value.to_string()),
            serde_json::Value::Number(value) => Some(value.to_string()),
            _ => None,
        }
    }
    let values = match value {
        serde_json::Value::Array(values) => values.iter().map(scalar).collect::<Option<Vec<_>>>(),
        value => scalar(value).map(|value| vec![value]),
    };
    metadata_values(field, values)
}

fn yaml_values(field: &str, value: &Yaml) -> Result<Vec<String>> {
    fn scalar(value: &Yaml) -> Option<String> {
        match value {
            Yaml::String(value) => Some(value.clone()),
            Yaml::Boolean(value) => Some(value.to_string()),
            Yaml::Integer(value) => Some(value.to_string()),
            Yaml::Real(value) => value
                .parse::<f64>()
                .ok()
                .and_then(serde_json::Number::from_f64)
                .map(|value| value.to_string()),
            _ => None,
        }
    }
    let values = match value {
        Yaml::Array(values) => values.iter().map(scalar).collect::<Option<Vec<_>>>(),
        value => scalar(value).map(|value| vec![value]),
    };
    metadata_values(field, values)
}

fn yaml_metadata(value: &Yaml) -> Result<Metadata> {
    let mut metadata = Metadata::new();
    if let Some(values) = value.as_hash() {
        for (field, value) in values {
            if let Some(field) = field.as_str().filter(|field| metadata_field(field)) {
                // Empty/unsupported explicit values still override inherited values.
                metadata.insert(field.to_owned(), yaml_values(field, value)?);
            }
        }
    }
    Ok(metadata)
}

fn metadata_string(metadata: &Metadata, field: &str) -> Option<String> {
    metadata
        .get(field)
        .and_then(|values| values.first())
        .cloned()
}

fn noindex(metadata: &Metadata) -> bool {
    metadata
        .iter()
        .filter(|(field, _)| field.eq_ignore_ascii_case("robots"))
        .flat_map(|(_, values)| values)
        .flat_map(|value| {
            value.split(|character: char| character == ',' || character.is_ascii_whitespace())
        })
        .any(|value| value.eq_ignore_ascii_case("noindex"))
}

fn completion_terms(title: &str, heading: &str) -> String {
    let mut terms = BTreeSet::new();
    for word in [title, heading]
        .into_iter()
        .flat_map(|text| text.split(|character: char| !character.is_alphanumeric()))
        .filter(|word| !word.is_empty())
    {
        let word = word.to_lowercase();
        let mut prefix = String::new();
        for (index, character) in word.chars().take(20).enumerate() {
            prefix.push(character);
            if index >= 1 {
                terms.insert(prefix.clone());
            }
        }
    }
    terms.into_iter().collect::<Vec<_>>().join(" ")
}

fn bounded_metadata(mut metadata: Metadata, workload: &str, path: &str) -> Result<Metadata> {
    metadata.remove("workload");
    metadata.remove("source_path");
    metadata.retain(|_, values| {
        values.retain(|value| metadata_value(value.clone()).is_some());
        !values.is_empty()
    });
    for (field, value) in [("workload", workload), ("source_path", path)] {
        let value = metadata_value(value.to_owned())
            .ok_or_else(|| invalid(format!("structural metadata exceeds value bounds: {path}")))?;
        metadata.insert(field.to_owned(), vec![value]);
    }
    if metadata.values().map(Vec::len).sum::<usize>() > MAX_METADATA_VALUES {
        return Err(invalid(format!(
            "article metadata exceeds {MAX_METADATA_VALUES} values: {path}"
        )));
    }
    Ok(metadata)
}

fn frontmatter(text: &str) -> Result<(Option<String>, Metadata, &str)> {
    let text = text.trim_start_matches('\u{feff}');
    let mut lines = text.split_inclusive('\n');
    if lines.next().map(str::trim) != Some("---") {
        return Ok((None, Metadata::new(), text));
    }
    let mut end = text.find('\n').map_or(text.len(), |index| index + 1);
    let start = end;
    for line in lines {
        if matches!(line.trim(), "---" | "...") {
            let yaml = YamlLoader::load_from_str(&text[start..end])?;
            let title = yaml
                .first()
                .and_then(|value| value["title"].as_str())
                .map(str::to_owned);
            let metadata = yaml
                .first()
                .map(yaml_metadata)
                .transpose()?
                .unwrap_or_default();
            return Ok((title, metadata, &text[end + line.len()..]));
        }
        end += line.len();
    }
    Err(invalid("unclosed YAML frontmatter"))
}

fn markdown_options() -> Options {
    Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_HEADING_ATTRIBUTES
}

#[derive(Default)]
struct Includes {
    unresolved: BTreeSet<String>,
    fragments: BTreeSet<String>,
}

impl Includes {
    fn expand(
        &mut self,
        snapshot: &mut Snapshot,
        path: &str,
        stack: &mut Vec<String>,
    ) -> Result<String> {
        if stack.len() >= MAX_INCLUDE_DEPTH || stack.iter().any(|item| item == path) {
            return Err(invalid(format!(
                "include depth limit or cycle at {path}: {}",
                stack.join(" -> ")
            )));
        }
        stack.push(path.to_owned());
        let raw = snapshot.read_text(path)?;
        let (_, _, text) = frontmatter(&raw)?;
        let line_base = raw[..raw.len() - text.len()]
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count();
        let protected: Vec<Range<usize>> = Parser::new_ext(text, markdown_options())
            .into_offset_iter()
            .filter_map(|(event, range)| match event {
                Event::Start(Tag::CodeBlock(_) | Tag::Image { .. })
                | Event::Code(_)
                | Event::Html(_)
                | Event::InlineHtml(_) => Some(range),
                _ => None,
            })
            .collect();
        let mut output = String::new();
        let mut offset = 0;
        let mut complex_image = false;
        for (line_number, line) in text.split_inclusive('\n').enumerate() {
            let is_protected = |position| protected.iter().any(|range| range.contains(&position));
            let first = offset + line.len() - line.trim_start().len();
            let trimmed = line.trim_start();
            if !is_protected(first) && trimmed.starts_with(":::image-end") {
                complex_image = false;
                output.push('\n');
            } else if complex_image {
                output.push('\n');
            } else if !is_protected(first) && trimmed.starts_with(":::image") {
                complex_image =
                    trimmed.contains("type=\"complex\"") || trimmed.contains("type='complex'");
                output.push('\n');
            } else if !is_protected(first) && trimmed.starts_with(":::") {
                output.push('\n');
            } else {
                let mut cursor = 0;
                while let Some(found) = line.as_bytes()[cursor..]
                    .windows(INCLUDE_DIRECTIVE.len())
                    .position(|window| window.eq_ignore_ascii_case(INCLUDE_DIRECTIVE.as_bytes()))
                    .map(|index| cursor + index)
                {
                    output.push_str(&line[cursor..found]);
                    if is_protected(offset + found)
                        || found > 0 && line.as_bytes()[found - 1] == b'\\'
                    {
                        cursor = found + INCLUDE_DIRECTIVE.len();
                        output.push_str(&line[found..cursor]);
                        continue;
                    }
                    let (consumed, target) = include_target(&line[found..])?;
                    if let Some(resolved) = include_path(path, target)? {
                        self.fragments.insert(resolved.clone());
                        output.push_str(&self.expand(snapshot, &resolved, stack)?);
                    } else {
                        self.unresolved
                            .insert(format!("{path}:{}: {target}", line_base + line_number + 1));
                        output.push('\n');
                    }
                    if output.len() > MAX_SOURCE_BYTES {
                        return Err(invalid(format!("expanded article exceeds 32 MiB: {path}")));
                    }
                    cursor = found + consumed;
                }
                output.push_str(&line[cursor..]);
            }
            if output.len() > MAX_SOURCE_BYTES {
                return Err(invalid(format!("expanded article exceeds 32 MiB: {path}")));
            }
            offset += line.len();
        }
        if complex_image {
            return Err(invalid(format!("unclosed complex image directive: {path}")));
        }
        stack.pop();
        Ok(output)
    }
}

fn include_target(text: &str) -> Result<(usize, &str)> {
    let rest = text
        .get(INCLUDE_DIRECTIVE.len()..)
        .ok_or_else(|| invalid("invalid include"))?
        .trim_start();
    let label = rest
        .strip_prefix('[')
        .ok_or_else(|| invalid("include needs a label"))?;
    let end = label
        .find(']')
        .ok_or_else(|| invalid("unclosed include label"))?;
    let destination = label[end + 1..]
        .trim_start()
        .strip_prefix('(')
        .ok_or_else(|| invalid("include needs a path"))?;
    let end = destination
        .find(')')
        .ok_or_else(|| invalid("unclosed include path"))?;
    let tail = &destination[end + 1..];
    let tail = tail.trim_start();
    if !tail.starts_with(']') {
        return Err(invalid("unclosed include directive"));
    }
    let consumed = text.len() - tail.len() + 1;
    let target = destination[..end].trim();
    Ok((
        consumed,
        target
            .strip_prefix('<')
            .and_then(|value| value.strip_suffix('>'))
            .unwrap_or(target),
    ))
}

fn include_path(origin: &str, target: &str) -> Result<Option<String>> {
    if target.starts_with("https://") || target.starts_with("http://") {
        return Ok(None);
    }
    if target.is_empty()
        || target.contains(['\\', ':', '%', '?', '#', '\0'])
        || target.starts_with('/')
        || target.chars().any(char::is_control)
    {
        return Err(invalid(format!("unsafe include path in {origin}")));
    }
    let (base, target) = if let Some(target) = target.strip_prefix("~/") {
        ("docs", target)
    } else {
        (
            origin.rsplit_once('/').map_or("", |(parent, _)| parent),
            target,
        )
    };
    let mut parts: Vec<_> = base.split('/').filter(|part| !part.is_empty()).collect();
    let mut escaped = false;
    for part in target.split('/') {
        match part {
            "." | "" => {}
            ".." => {
                if parts.pop().is_none() {
                    escaped = true;
                }
            }
            _ => parts.push(part),
        }
    }
    if parts
        .first()
        .is_some_and(|part| DEPENDENCIES.contains(part))
    {
        return Ok(None);
    }
    if escaped {
        return Err(invalid(format!("include escapes checkout: {origin}")));
    }
    let resolved = parts.join("/");
    validate_repo_path(&resolved)?;
    if extension(&resolved) != "md" {
        return Err(invalid(format!(
            "include is not a Markdown fragment: {origin}"
        )));
    }
    Ok(Some(resolved))
}

struct Article {
    title: String,
    sections: Vec<Section>,
}

struct Section {
    ordinal: usize,
    heading: String,
    body: String,
    heading_path: Vec<String>,
    anchor: Option<String>,
}

fn markdown_article(title: Option<String>, text: &str) -> Article {
    let mut sections = Vec::new();
    let mut current = Section {
        ordinal: 0,
        heading: String::new(),
        body: String::new(),
        heading_path: Vec::new(),
        anchor: None,
    };
    let mut heading = None;
    let mut heading_level = 0;
    let mut ancestors: Vec<(u8, String)> = Vec::new();
    let mut pending_anchor: Option<(String, usize)> = None;
    let mut image_depth = 0usize;
    let mut html = HtmlText::default();
    for (event, range) in Parser::new_ext(text, markdown_options()).into_offset_iter() {
        match event {
            Event::Start(Tag::Image { .. }) => {
                image_depth += 1;
                continue;
            }
            Event::End(TagEnd::Image) => {
                image_depth = image_depth.saturating_sub(1);
                continue;
            }
            _ if image_depth > 0 => continue,
            _ => {}
        }
        match event {
            Event::Start(Tag::Heading { level, id, .. }) => {
                let ordinal = current.ordinal + 1;
                finish_section(&mut sections, current);
                heading_level = level as u8;
                let adjacent_anchor = pending_anchor.take().and_then(|(anchor, end)| {
                    (end <= range.start && text[end..range.start].trim().is_empty())
                        .then_some(anchor)
                });
                current = Section {
                    ordinal,
                    heading: String::new(),
                    body: String::new(),
                    heading_path: Vec::new(),
                    anchor: id
                        .filter(|value| !value.is_empty())
                        .map(|value| value.into_string())
                        .or(adjacent_anchor),
                };
                heading = Some(String::new());
            }
            Event::End(TagEnd::Heading(_)) => {
                current.heading = heading.take().unwrap_or_default().trim().to_owned();
                while ancestors
                    .last()
                    .is_some_and(|(level, _)| *level >= heading_level)
                {
                    ancestors.pop();
                }
                ancestors.push((heading_level, current.heading.clone()));
                current.heading_path = ancestors.iter().map(|(_, text)| text.clone()).collect();
            }
            Event::Text(text)
            | Event::Code(text)
            | Event::InlineMath(text)
            | Event::DisplayMath(text) => {
                if let Some(heading) = &mut heading {
                    heading.push_str(&text);
                } else {
                    current.body.push_str(&text);
                }
            }
            Event::Html(markup) | Event::InlineHtml(markup) => {
                let start = range.start + text[range.start..].len()
                    - text[range.start..].trim_start().len();
                if start < range.end
                    && let Some((anchor, consumed)) = html_anchor(&text[start..])
                {
                    if heading.is_some() {
                        if current.anchor.is_none() {
                            current.anchor = Some(anchor);
                        }
                    } else if text[..start]
                        .rsplit('\n')
                        .next()
                        .is_some_and(|line| line.trim().is_empty())
                    {
                        pending_anchor = Some((anchor, start + consumed));
                    }
                }
                let visible = html.extract(&markup);
                if let Some(heading) = &mut heading {
                    heading.push_str(&visible);
                } else {
                    current.body.push_str(&visible);
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                if let Some(heading) = &mut heading {
                    heading.push(' ');
                } else {
                    current.body.push('\n');
                }
            }
            Event::Start(Tag::CodeBlock(_))
            | Event::End(
                TagEnd::Paragraph
                | TagEnd::CodeBlock
                | TagEnd::Item
                | TagEnd::TableRow
                | TagEnd::HtmlBlock,
            )
            | Event::Rule => current.body.push('\n'),
            Event::End(TagEnd::TableCell) => current.body.push(' '),
            _ => {}
        }
    }
    finish_section(&mut sections, current);
    let title = title
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| {
            sections
                .iter()
                .find(|section| !section.heading.is_empty())
                .map_or_else(
                    || "Microsoft Fabric".to_owned(),
                    |section| section.heading.clone(),
                )
        });
    let title = plain_text(&title);
    for section in &mut sections {
        if section.heading.is_empty() {
            section.heading = title.clone();
        }
    }
    Article { title, sections }
}

fn html_anchor(text: &str) -> Option<(String, usize)> {
    let bytes = text.as_bytes();
    if bytes.first() != Some(&b'<')
        || !bytes.get(1)?.eq_ignore_ascii_case(&b'a')
        || !bytes.get(2)?.is_ascii_whitespace()
    {
        return None;
    }
    let mut position = 2;
    let mut anchor = None;
    loop {
        while bytes.get(position).is_some_and(u8::is_ascii_whitespace) {
            position += 1;
        }
        if bytes.get(position) == Some(&b'>') {
            position += 1;
            break;
        }
        let start = position;
        while bytes
            .get(position)
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || b"-_:".contains(byte))
        {
            position += 1;
        }
        if position == start {
            return None;
        }
        let name = &text[start..position];
        while bytes.get(position).is_some_and(u8::is_ascii_whitespace) {
            position += 1;
        }
        if bytes.get(position) != Some(&b'=') {
            if name.eq_ignore_ascii_case("id") {
                return None;
            }
            continue;
        }
        position += 1;
        while bytes.get(position).is_some_and(u8::is_ascii_whitespace) {
            position += 1;
        }
        let quote = bytes.get(position).copied()?;
        let value = if matches!(quote, b'\'' | b'"') {
            position += 1;
            let start = position;
            while bytes.get(position).is_some_and(|byte| *byte != quote) {
                position += 1;
            }
            bytes.get(position)?;
            let value = &text[start..position];
            position += 1;
            value
        } else {
            let start = position;
            while bytes
                .get(position)
                .is_some_and(|byte| !byte.is_ascii_whitespace() && *byte != b'>')
            {
                position += 1;
            }
            &text[start..position]
        };
        if name.eq_ignore_ascii_case("id") {
            if anchor.is_some()
                || value.is_empty()
                || value.contains('&')
                || value
                    .chars()
                    .any(|character| character.is_whitespace() || character.is_control())
            {
                return None;
            }
            anchor = Some(value.to_owned());
        }
    }
    let closing = text[position..].trim_start();
    if !closing.get(..4)?.eq_ignore_ascii_case("</a>") {
        return None;
    }
    Some((anchor?, text.len() - closing.len() + 4))
}

fn finish_section(sections: &mut Vec<Section>, mut section: Section) {
    section.body = if section.body.trim().is_empty() {
        String::new()
    } else {
        section.body.trim_matches(['\r', '\n']).to_owned()
    };
    if !section.body.is_empty() || !section.heading.is_empty() {
        sections.push(section);
    }
}

fn plain_text(text: &str) -> String {
    let mut plain = String::new();
    let mut image_depth = 0usize;
    let mut html = HtmlText::default();
    for event in Parser::new_ext(text, markdown_options()) {
        match event {
            Event::Start(Tag::Image { .. }) => image_depth += 1,
            Event::End(TagEnd::Image) => image_depth = image_depth.saturating_sub(1),
            _ if image_depth > 0 => {}
            Event::Text(text) | Event::Code(text) => plain.push_str(&text),
            Event::Html(text) | Event::InlineHtml(text) => plain.push_str(&html.extract(&text)),
            Event::SoftBreak | Event::HardBreak | Event::End(TagEnd::Paragraph) => plain.push(' '),
            _ => {}
        }
    }
    plain.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[derive(Default)]
struct HtmlText {
    comment: bool,
    hidden: Option<String>,
}

impl HtmlText {
    fn extract(&mut self, mut text: &str) -> String {
        let mut output = String::new();
        while !text.is_empty() {
            if self.comment {
                let Some(end) = text.find("-->") else { break };
                self.comment = false;
                text = &text[end + 3..];
            } else if text.starts_with("<!--") {
                self.comment = true;
                text = &text[4..];
            } else if text.starts_with('<') {
                let Some(end) = text.find('>') else { break };
                let tag = text[1..end].trim().to_ascii_lowercase();
                let name = tag.split_whitespace().next().unwrap_or("");
                if matches!(name, "script" | "style" | "svg") {
                    self.hidden = Some(name.to_owned());
                } else if self
                    .hidden
                    .as_ref()
                    .is_some_and(|hidden| name == format!("/{hidden}"))
                {
                    self.hidden = None;
                }
                output.push(' ');
                text = &text[end + 1..];
            } else {
                let end = text.find('<').unwrap_or(text.len());
                if self.hidden.is_none() {
                    output.push_str(&text[..end]);
                }
                text = &text[end..];
            }
        }
        output
    }
}

fn landing_article(text: &str, metadata: &mut Metadata) -> Result<Option<Article>> {
    let mime = text
        .lines()
        .find(|line| line.trim_start().starts_with("### YamlMime:"))
        .and_then(|line| line.split_once("YamlMime:"))
        .map(|(_, mime)| mime.trim());
    if !matches!(mime, Some("Landing" | "Hub")) {
        return Ok(None);
    }
    let yaml = YamlLoader::load_from_str(text)?;
    let root = yaml
        .first()
        .ok_or_else(|| invalid("empty YAML landing document"))?;
    let title = root["title"]
        .as_str()
        .ok_or_else(|| invalid("YAML landing page needs a title"))?;
    metadata.extend(yaml_metadata(&root["metadata"])?);
    metadata.insert("title".to_owned(), yaml_values("title", &root["title"])?);
    let mut content = String::new();
    fn collect(value: &Yaml, text: &mut String) {
        match value {
            Yaml::Array(values) => {
                for value in values {
                    collect(value, text);
                }
            }
            Yaml::Hash(values) => {
                for (key, value) in values {
                    match key.as_str() {
                        Some("title" | "summary" | "description" | "text") => {
                            if let Some(value) = value.as_str() {
                                text.push_str(value);
                                text.push_str("\n\n");
                            }
                        }
                        Some(
                            "metadata" | "image" | "imageSrc" | "thumbnailUrl" | "url" | "href"
                            | "altText" | "icon",
                        ) => {}
                        _ => collect(value, text),
                    }
                }
            }
            _ => {}
        }
    }
    collect(root, &mut content);
    Ok(Some(markdown_article(Some(title.to_owned()), &content)))
}

fn article_url(path: &str) -> String {
    let path = path.rsplit_once('.').map_or(path, |(stem, _)| stem);
    let path = if path == "index" {
        String::new()
    } else {
        path.strip_suffix("/index")
            .map_or_else(|| path.to_owned(), |parent| format!("{parent}/"))
    };
    format!("https://learn.microsoft.com/fabric/{}", url_path(&path))
}

fn url_path(path: &str) -> String {
    path.bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || b"/-._~".contains(&byte) {
                char::from(byte).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #![deny(unnameable_test_items)]

    use super::*;

    #[test]
    fn reviewed_item_types_are_revision_and_profile_bound_exact_classifications() -> Result<()> {
        let map = ItemTypeMap::parse(include_str!("../data/item-types.json"))?;
        assert_eq!(map.articles.len(), 10);
        for profile in ["metadata", "full"] {
            let features = IndexFeatures::profile(profile).ok_or("missing fixture profile")?;
            assert!(reviewed_item_types(&map.source_commit, features)?.is_some());
            assert!(
                reviewed_item_types("0000000000000000000000000000000000000000", features)?
                    .is_none()
            );
        }
        for profile in ["baseline", "stemmed", "hierarchy", "autocomplete"] {
            let features = IndexFeatures::profile(profile).ok_or("missing fixture profile")?;
            assert!(reviewed_item_types(&map.source_commit, features)?.is_none());
        }

        let path = "docs/data-engineering/lakehouse-overview.md";
        let mut metadata = Metadata::new();
        assert!(map.apply(path, &mut metadata)?);
        assert_eq!(metadata["item_type"], ["Lakehouse"]);
        assert!(!metadata.contains_key("item_type.provenance"));
        let original = metadata.clone();
        assert!(map.apply(path, &mut metadata)?);
        assert_eq!(metadata, original);

        for path in [
            "docs/data-engineering/lakehouse-guide.md",
            "docs/data-engineering/lakehouse-overview.md/child.md",
            "docs/unrelated/lakehouse-overview.md",
        ] {
            let mut unknown = Metadata::new();
            assert!(!map.apply(path, &mut unknown)?);
            assert!(unknown.is_empty());
        }
        for field in ["item_type", "itemType", "ms.item_type"] {
            let mut conflicting =
                Metadata::from([(field.to_owned(), vec!["Warehouse".to_owned()])]);
            let original = conflicting.clone();
            assert!(map.apply(path, &mut conflicting).is_err());
            assert_eq!(conflicting, original);
        }

        let mut entries = map
            .articles
            .keys()
            .map(|path| {
                (
                    path.clone(),
                    Entry {
                        mode: "100644".to_owned(),
                        object: "0000000000000000000000000000000000000000".to_owned(),
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();
        map.validate_paths(&entries)?;
        entries.get_mut(path).ok_or("missing fixture path")?.mode = "120000".to_owned();
        assert!(map.validate_paths(&entries).is_err());
        entries.remove(path);
        assert!(map.validate_paths(&entries).is_err());
        let notice = map.attribution();
        assert!(notice.contains("data/item-types.json"));
        assert!(notice.contains(&map.source_commit));
        assert!(notice.contains(&map.articles[path].evidence));
        assert!(notice.contains("not original article frontmatter"));
        Ok(())
    }

    #[test]
    fn reviewed_item_type_map_rejects_malformed_or_unbounded_entries() -> Result<()> {
        let value: serde_json::Value =
            serde_json::from_str(include_str!("../data/item-types.json"))?;
        let path = "docs/data-engineering/lakehouse-overview.md";
        for replacement in [
            serde_json::json!([]),
            serde_json::json!([""]),
            serde_json::json!([" Lakehouse"]),
            serde_json::json!(["Lakehouse\n"]),
            serde_json::json!(["Lakehouse", "Lakehouse"]),
            serde_json::json!(["x".repeat(513)]),
            serde_json::json!([42]),
        ] {
            let mut invalid = value.clone();
            invalid["articles"][path]["item_types"] = replacement;
            assert!(ItemTypeMap::parse(&invalid.to_string()).is_err());
        }
        for (field, replacement) in [
            ("schema_version", serde_json::json!(2)),
            ("source_commit", serde_json::json!("main")),
            ("source_commit", serde_json::json!("A".repeat(40))),
        ] {
            let mut invalid = value.clone();
            invalid[field] = replacement;
            assert!(ItemTypeMap::parse(&invalid.to_string()).is_err());
        }
        let mut invalid = value.clone();
        invalid["articles"][path]["evidence"] = serde_json::json!("");
        assert!(ItemTypeMap::parse(&invalid.to_string()).is_err());
        let mut invalid = value;
        let articles = invalid["articles"]
            .as_object_mut()
            .ok_or("missing fixture articles")?;
        let classification = articles.remove(path).ok_or("missing fixture article")?;
        articles.insert("docs/../outside.md".to_owned(), classification);
        assert!(ItemTypeMap::parse(&invalid.to_string()).is_err());
        Ok(())
    }
}
