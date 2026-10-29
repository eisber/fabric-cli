// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Pinned scalar properties and two deliberately distinct width policies.
//!
//! ICU4X supplies complete current Unicode properties. Generated CPython/Rich
//! compatibility data supplies the *source-version* decisions; upgrading ICU
//! must not silently change Python repr, casing, or table layout.
//!
//! `CharacterWidth` is the actual existing output interface: source tables add
//! East Asian F/W widths, including one cell for controls and combining marks.
//! [`ProductionUnicode::rich_text_width`] instead implements Rich's measured
//! sequence policy. Neither policy is a general UAX #29 grapheme segmenter.

use crate::text::{self, CharacterWidth, TextError, UnicodeDataKind, UnicodeResourceKind};
use icu_properties::{
    CodePointMapData, CodePointSetData,
    props::{
        DefaultIgnorableCodePoint, EastAsianWidth, Emoji, EmojiPresentation, ExtendedPictographic,
        GeneralCategory, GraphemeClusterBreak,
    },
};
use serde::Deserialize;
use serde_json::Value;
use std::sync::LazyLock;
use thiserror::Error;

const SOURCE_COMMIT: &str = "cf67fd52ea8b47e24c725e73d792de6f4a08e2e4";
const SCALAR_COUNT: usize = 0x110000 - 0x800;
const MAX_DATA_BYTES: usize = 4 * 1024 * 1024;
const MAX_RANGES: usize = 32768;
const MAX_MAPPINGS: usize = 8192;
const PRINTABLE: u8 = 1;
const ALPHANUMERIC: u8 = 2;
const UPPERCASE: u8 = 4;
const CASED: u8 = 8;
const CASE_IGNORABLE: u8 = 16;
const WHITESPACE: u8 = 32;

/// Compatibility input and output limits; no unbounded formatter mode exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnicodeLimits {
    /// Aggregate UTF-8 bytes of strings visited during one operation.
    pub input_bytes: usize,
    /// UTF-8 bytes produced by one operation.
    pub output_bytes: usize,
    /// JSON nesting depth, with the root at zero.
    pub depth: usize,
    /// JSON nodes, including object keys, visited during one operation.
    pub nodes: usize,
}

impl Default for UnicodeLimits {
    fn default() -> Self {
        Self {
            input_bytes: 4 * 1024 * 1024,
            output_bytes: 16 * 1024 * 1024,
            depth: 128,
            nodes: 1_000_000,
        }
    }
}

/// Initialization failures retain data errors separately from operation failures.
#[derive(Debug, Error)]
pub enum UnicodeDataError {
    /// Generated data is incomplete, malformed, or from an unapproved version.
    #[error("invalid pinned Unicode data: {0}")]
    Invalid(#[source] UnicodeDataKind),
    /// The caller supplied malformed JSON for the generated data.
    #[error("invalid pinned Unicode data JSON")]
    Json(#[source] serde_json::Error),
    /// The encoded property database exceeds its finite input budget.
    #[error("Unicode {0} limit exceeded")]
    Resource(#[source] UnicodeResourceKind),
}

impl UnicodeDataError {
    /// Classify initialization failure for ordinary typed text error propagation.
    ///
    /// The original serde error remains available through this error's source
    /// and [`embedded_provider`]; callback diagnostics do not echo data contents.
    pub fn to_text_error(&self) -> TextError {
        match self {
            Self::Invalid(kind) => TextError::UnicodeData(*kind),
            Self::Json(_) => TextError::UnicodeData(UnicodeDataKind::MalformedJson),
            Self::Resource(kind) => TextError::UnicodeResource(*kind),
        }
    }
}

/// Typed operation failures, separate from initialization and JSON encoders.
#[derive(Debug, Error)]
pub enum UnicodeError {
    /// The caller supplied a surrogate or a value above U+10FFFF.
    #[error("U+{0:04X} is not a Unicode scalar")]
    InvalidScalar(u32),
    /// A finite operation budget was exhausted before producing a result.
    #[error("Unicode {0} limit exceeded")]
    Limit(#[source] UnicodeResourceKind),
    /// Original source key validation or existing numeric formatting failed.
    #[error(transparent)]
    Text(#[from] TextError),
}

impl From<UnicodeError> for TextError {
    fn from(error: UnicodeError) -> Self {
        match error {
            UnicodeError::Text(error) => error,
            UnicodeError::Limit(kind) => Self::UnicodeResource(kind),
            UnicodeError::InvalidScalar(point) => {
                Self::UnicodeData(UnicodeDataKind::InvalidScalar(point))
            }
        }
    }
}

/// CPython 3.13.15 / UCD 15.1 scalar decisions, not the current ICU database.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceScalarProperties {
    /// Python's East Asian width class.
    pub east_asian_width: &'static str,
    /// Python's two-letter general category.
    pub general_category: &'static str,
    /// Python `str.isprintable()` for this scalar.
    pub printable: bool,
    /// Python `str.isalnum()` for this scalar.
    pub alphanumeric: bool,
    /// Python `str.isupper()` for this scalar.
    pub uppercase: bool,
    /// Python's Cased predicate, used by `str.title()`.
    pub cased: bool,
    /// Python's Case_Ignorable predicate, used by contextual final sigma.
    pub case_ignorable: bool,
    /// Python `str.isspace()`, including its additional C0 separators.
    pub whitespace: bool,
    /// Source `fab_ui._char_visual_width`: always one or two.
    pub source_width: usize,
    /// Rich 14.3.3 scalar width with its explicit Unicode 17.0.0 table.
    pub rich_width: usize,
}

/// Current ICU4X 2.4.0 properties, explicitly separate from source compatibility.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IcuScalarProperties {
    /// Unicode 18.0 East_Asian_Width; not automatically a terminal width.
    pub east_asian_width: EastAsianWidth,
    /// Unicode 18.0 General_Category.
    pub general_category: GeneralCategory,
    /// Unicode 18.0 Grapheme_Cluster_Break; not a segmentation algorithm.
    pub grapheme_cluster_break: GraphemeClusterBreak,
    /// Default_Ignorable_Code_Point; does not by itself imply source width zero.
    pub default_ignorable: bool,
    /// Emoji property.
    pub emoji: bool,
    /// Emoji_Presentation property.
    pub emoji_presentation: bool,
    /// Extended_Pictographic property.
    pub extended_pictographic: bool,
}

// Each range is inclusive: start, end, EAW index, category index, flags, Rich width.
#[derive(Debug, Deserialize)]
struct ScalarRange(u32, u32, u8, u8, u8, u8);

// Complete non-identity mappings from the pinned interpreter, not Rust casing.
#[derive(Debug, Deserialize)]
struct CaseMapping(u32, String, String);

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PinnedData {
    schema: String,
    source_commit: String,
    python_version: String,
    python_ucd: String,
    rich_version: String,
    rich_ucd: String,
    scalar_count: usize,
    scalar_fnv64: String,
    ranges_sha256: String,
    copyright: String,
    licenses: Vec<String>,
    ranges: Vec<ScalarRange>,
    case_mappings: Vec<CaseMapping>,
    rich_narrow_to_wide: Vec<u32>,
}

const EAW: [&str; 6] = ["N", "Na", "A", "H", "W", "F"];
const CATEGORIES: [&str; 30] = [
    "Cn", "Lu", "Ll", "Lt", "Lm", "Lo", "Mn", "Me", "Mc", "Nd", "Nl", "No", "Zs", "Zl", "Zp", "Cc",
    "Cf", "Co", "Cs", "Pd", "Ps", "Pe", "Pc", "Po", "Sm", "Sc", "Sk", "So", "Pi", "Pf",
];

/// Immutable, caller-injected complete scalar database and bounded text formatter.
///
/// Construction accepts only a complete generated source database. The caller
/// should embed the reviewed artifact, not read an ambient file. No Python,
/// environment lookup, terminal probing, data download, or mutable global is used.
#[derive(Debug)]
pub struct ProductionUnicode {
    data: PinnedData,
    limits: UnicodeLimits,
}

impl ProductionUnicode {
    /// Validate and load a reviewed generated artifact, within fixed size limits.
    ///
    /// Structural validation proves complete scalar coverage, not provenance:
    /// independent Python verification and review of the artifact hash remain
    /// required. Caller limits may tighten, but never exceed, the default caps.
    pub fn from_json(json: &str, limits: UnicodeLimits) -> Result<Self, UnicodeDataError> {
        if json.len() > MAX_DATA_BYTES {
            return Err(UnicodeDataError::Resource(UnicodeResourceKind::DataBytes));
        }
        let caps = UnicodeLimits::default();
        if limits.input_bytes > caps.input_bytes
            || limits.output_bytes > caps.output_bytes
            || limits.depth > caps.depth
            || limits.nodes > caps.nodes
        {
            return Err(UnicodeDataError::Invalid(UnicodeDataKind::Limits));
        }
        let data: PinnedData = serde_json::from_str(json).map_err(UnicodeDataError::Json)?;
        validate(&data)?;
        Ok(Self { data, limits })
    }

    /// Source-data version used for Python-specific decisions.
    pub const fn source_unicode_version(&self) -> &'static str {
        "15.1.0"
    }

    /// Compiled ICU data version, from icu_properties_data 2.4.0 metadata.
    pub const fn icu_unicode_version(&self) -> &'static str {
        "18.0.0"
    }

    /// Rich data version selected explicitly, never by its `auto` environment path.
    pub const fn rich_unicode_version(&self) -> &'static str {
        "17.0.0"
    }

    /// Generated scalar-stream checksum for independent exhaustive verification.
    pub fn scalar_checksum(&self) -> &str {
        &self.data.scalar_fnv64
    }

    /// SHA-256 of canonical range JSON, recorded by the independent generator.
    pub fn ranges_checksum(&self) -> &str {
        &self.data.ranges_sha256
    }

    /// Query any integer code point, rejecting surrogates and out-of-range values.
    pub fn codepoint(&self, codepoint: u32) -> Result<SourceScalarProperties, UnicodeError> {
        let character = char::from_u32(codepoint).ok_or(UnicodeError::InvalidScalar(codepoint))?;
        Ok(self.properties(character))
    }

    /// Query every valid scalar, including NUL, private use and noncharacters.
    pub fn properties(&self, character: char) -> SourceScalarProperties {
        let point = u32::from(character);
        let index = self.data.ranges.partition_point(|range| range.1 < point);
        let range = &self.data.ranges[index];
        SourceScalarProperties {
            east_asian_width: EAW[usize::from(range.2)],
            general_category: CATEGORIES[usize::from(range.3)],
            printable: range.4 & PRINTABLE != 0,
            alphanumeric: range.4 & ALPHANUMERIC != 0,
            uppercase: range.4 & UPPERCASE != 0,
            cased: range.4 & CASED != 0,
            case_ignorable: range.4 & CASE_IGNORABLE != 0,
            whitespace: range.4 & WHITESPACE != 0,
            source_width: if range.2 >= 4 { 2 } else { 1 },
            rich_width: usize::from(range.5),
        }
    }

    /// Query ICU's compiled current properties without applying Python adapters.
    pub fn icu_properties(&self, character: char) -> IcuScalarProperties {
        IcuScalarProperties {
            east_asian_width: CodePointMapData::<EastAsianWidth>::new().get(character),
            general_category: CodePointMapData::<GeneralCategory>::new().get(character),
            grapheme_cluster_break: CodePointMapData::<GraphemeClusterBreak>::new().get(character),
            default_ignorable: CodePointSetData::new::<DefaultIgnorableCodePoint>()
                .contains(character),
            emoji: CodePointSetData::new::<Emoji>().contains(character),
            emoji_presentation: CodePointSetData::new::<EmojiPresentation>().contains(character),
            extended_pictographic: CodePointSetData::new::<ExtendedPictographic>()
                .contains(character),
        }
    }

    /// Measure source tables by additive scalar F/W width, not UTF-8 byte length.
    pub fn source_text_width(&self, text: &str) -> Result<usize, UnicodeError> {
        self.check_input(text)?;
        Ok(text.chars().map(|c| self.properties(c).source_width).sum())
    }

    /// Measure exactly Rich 14.3.3's explicit Unicode 17.0.0 sequence algorithm.
    ///
    /// Combining/control/NUL widths come from Rich's generated data. ZWJ skips
    /// the following scalar, even in malformed sequences; VS16 can widen the
    /// last nonzero-width scalar once. This intentionally does not substitute a
    /// UAX #29 segmenter or `unicode-width` implementation for the source.
    pub fn rich_text_width(&self, text: &str) -> Result<usize, UnicodeError> {
        self.check_input(text)?;
        let mut characters = text.chars();
        let mut total = 0;
        let mut last = None;
        while let Some(character) = characters.next() {
            match character {
                '\u{200d}' => {
                    characters.next();
                }
                '\u{fe0f}' => {
                    if let Some(previous) = last.take() {
                        total += usize::from(
                            self.data
                                .rich_narrow_to_wide
                                .binary_search(&u32::from(previous))
                                .is_ok(),
                        );
                    }
                }
                _ => {
                    let width = self.properties(character).rich_width;
                    if width != 0 {
                        last = Some(character);
                        total += width;
                    }
                }
            }
        }
        Ok(total)
    }

    /// Python's source key validation, full title expansions and contextual sigma.
    pub fn title_key(&self, key: &str) -> Result<String, UnicodeError> {
        self.check_input(key)?;
        let mut stripped = key.chars().filter(|c| !matches!(c, '_' | ' ')).peekable();
        if stripped.peek().is_none()
            || !stripped.all(|c| self.properties(c).alphanumeric)
            || (key.contains(' ') && key.contains('_'))
            || (!key.contains('_') && key.chars().skip(1).any(|c| self.properties(c).uppercase))
        {
            return Err(TextError::InvalidKey(key.to_owned()).into());
        }
        let characters: Vec<_> = key
            .chars()
            .map(|c| if c == '_' { ' ' } else { c })
            .collect();
        let mut next_cased = vec![false; characters.len() + 1];
        for index in (0..characters.len()).rev() {
            let properties = self.properties(characters[index]);
            next_cased[index] = if properties.case_ignorable {
                next_cased[index + 1]
            } else {
                properties.cased
            };
        }
        let mut output = BoundedOutput::new(self.limits);
        let mut previous_cased = false;
        let mut preceding_cased = false;
        for (index, &character) in characters.iter().enumerate() {
            let properties = self.properties(character);
            if previous_cased && character == '\u{03a3}' {
                output.character(if preceding_cased && !next_cased[index + 1] {
                    '\u{03c2}'
                } else {
                    '\u{03c3}'
                })?;
            } else if let Some(mapping) = self.mapping(character) {
                output.push(if previous_cased {
                    &mapping.1
                } else {
                    &mapping.2
                })?;
            } else {
                output.character(character)?;
            }
            previous_cased = properties.cased;
            if !properties.case_ignorable {
                preceding_cased = properties.cased;
            }
        }
        let mut pretty = output
            .text
            .trim_matches(|c| self.properties(c).whitespace)
            .to_owned();
        for (from, to) in [("Id", "ID"), ("Cli", "CLI"), ("Powerbi", "PowerBI")] {
            let mut replaced = BoundedOutput::new(self.limits);
            let mut rest = pretty.as_str();
            while let Some(index) = rest.find(from) {
                replaced.push(&rest[..index])?;
                replaced.push(to)?;
                rest = &rest[index + from.len()..];
            }
            replaced.push(rest)?;
            pretty = replaced.text;
        }
        Ok(pretty)
    }

    /// Python str/repr for the existing JSON value and numeric domain.
    ///
    /// Top-level strings stay raw. Nested strings use pinned CPython printability,
    /// quote selection and lowercase x/u/U escapes. Numbers delegate unchanged
    /// to the existing formatter. This neither broadens JSON to surrogates or
    /// nonfinite numbers nor changes any JSON serialization entry point.
    pub fn python_string(&self, value: &Value) -> Result<String, UnicodeError> {
        let mut output = BoundedOutput::new(self.limits);
        self.write_value(value, false, 0, &mut output)?;
        Ok(output.text)
    }

    fn check_input(&self, text: &str) -> Result<(), UnicodeError> {
        if text.len() > self.limits.input_bytes {
            Err(UnicodeError::Limit(UnicodeResourceKind::InputBytes))
        } else {
            Ok(())
        }
    }

    fn mapping(&self, character: char) -> Option<&CaseMapping> {
        self.data
            .case_mappings
            .binary_search_by_key(&u32::from(character), |mapping| mapping.0)
            .ok()
            .map(|index| &self.data.case_mappings[index])
    }

    fn write_value(
        &self,
        value: &Value,
        nested: bool,
        depth: usize,
        output: &mut BoundedOutput,
    ) -> Result<(), UnicodeError> {
        output.enter(depth)?;
        match value {
            Value::String(value) => {
                output.input(value)?;
                if nested {
                    self.write_repr(value, output)?;
                } else {
                    output.push(value)?;
                }
            }
            Value::Array(values) => {
                output.push("[")?;
                for (index, value) in values.iter().enumerate() {
                    if index != 0 {
                        output.push(", ")?;
                    }
                    self.write_value(value, true, depth + 1, output)?;
                }
                output.push("]")?;
            }
            Value::Object(values) => {
                output.push("{")?;
                for (index, (key, value)) in values.iter().enumerate() {
                    if index != 0 {
                        output.push(", ")?;
                    }
                    output.enter(depth + 1)?;
                    output.input(key)?;
                    self.write_repr(key, output)?;
                    output.push(": ")?;
                    self.write_value(value, true, depth + 1, output)?;
                }
                output.push("}")?;
            }
            _ => output.push(&text::python_string(value)?)?,
        }
        Ok(())
    }

    fn write_repr(&self, value: &str, output: &mut BoundedOutput) -> Result<(), UnicodeError> {
        let quote = if value.contains('\'') && !value.contains('"') {
            '"'
        } else {
            '\''
        };
        output.character(quote)?;
        for character in value.chars() {
            match character {
                '\\' => output.push("\\\\")?,
                '\t' => output.push("\\t")?,
                '\r' => output.push("\\r")?,
                '\n' => output.push("\\n")?,
                c if c == quote => {
                    output.push("\\")?;
                    output.character(c)?;
                }
                c if self.properties(c).printable => output.character(c)?,
                c => {
                    let point = u32::from(c);
                    output.push(&if point <= 0xff {
                        format!("\\x{point:02x}")
                    } else if point <= 0xffff {
                        format!("\\u{point:04x}")
                    } else {
                        format!("\\U{point:08x}")
                    })?;
                }
            }
        }
        output.character(quote)
    }
}

impl CharacterWidth for ProductionUnicode {
    fn width(&self, character: char) -> Option<usize> {
        Some(self.properties(character).source_width)
    }
}

static EMBEDDED_PROVIDER: LazyLock<Result<ProductionUnicode, UnicodeDataError>> =
    LazyLock::new(|| {
        ProductionUnicode::from_json(
            include_str!("../tests/production_unicode/source-data.json"),
            UnicodeLimits::default(),
        )
    });

/// Return the immutable embedded provider or its original initialization error.
///
/// The database is decoded once from reviewed compiled-in data. There is no
/// environment selection, mutable configuration, downloaded data or fallback.
/// A failed initialization is retained as an error rather than poisoning the
/// lazy initializer. The original serde cause is available through `Error::source`.
pub fn embedded_provider() -> Result<&'static ProductionUnicode, &'static UnicodeDataError> {
    EMBEDDED_PROVIDER.as_ref()
}

/// Obtain the source scalar-width provider with ordinary typed text errors.
///
/// Call this before passing the provider to a table renderer. The scalar-only
/// [`CharacterWidth`] trait cannot represent initialization failures itself.
pub fn width_provider() -> Result<&'static ProductionUnicode, TextError> {
    embedded_provider().map_err(UnicodeDataError::to_text_error)
}

/// Default source-compatible key callback for [`text::TextOptions`].
///
/// Source key errors and finite-budget failures propagate without relabeling
/// them as JSON failures or retrying with an ASCII formatter.
pub fn key_formatter(key: &str) -> Result<String, TextError> {
    width_provider()?.title_key(key).map_err(TextError::from)
}

/// Default source-compatible str/repr callback for [`text::TextOptions`].
///
/// The accepted numeric and string domains remain those of the existing
/// `serde_json::Value` API; this callback never invokes the JSON serializer.
pub fn value_formatter(value: &Value) -> Result<String, TextError> {
    width_provider()?
        .python_string(value)
        .map_err(TextError::from)
}

fn validate(data: &PinnedData) -> Result<(), UnicodeDataError> {
    if data.schema != "fabric-output-unicode-v1"
        || data.source_commit != SOURCE_COMMIT
        || data.python_version != "3.13.15"
        || data.python_ucd != "15.1.0"
        || data.rich_version != "14.3.3"
        || data.rich_ucd != "17.0.0"
        || data.scalar_count != SCALAR_COUNT
        || data.scalar_fnv64.len() != 16
        || !data.scalar_fnv64.bytes().all(|c| c.is_ascii_hexdigit())
        || data.ranges_sha256.len() != 64
        || !data.ranges_sha256.bytes().all(|c| c.is_ascii_hexdigit())
        || data.copyright != "Copyright (c) Microsoft Corporation."
        || data.licenses != ["MIT", "Unicode-3.0", "PSF-2.0"]
    {
        return Err(UnicodeDataError::Invalid(UnicodeDataKind::Version));
    }
    if data.ranges.len() > MAX_RANGES || data.case_mappings.len() > MAX_MAPPINGS {
        return Err(UnicodeDataError::Invalid(UnicodeDataKind::TableSize));
    }
    let mut next = 0;
    let mut covered = 0;
    for range in &data.ranges {
        if range.0 != next
            || range.1 < range.0
            || range.1 > 0x10ffff
            || (range.0 <= 0xdfff && range.1 >= 0xd800)
            || usize::from(range.2) >= EAW.len()
            || usize::from(range.3) >= CATEGORIES.len()
            || range.3 == 18
            || range.4 > 63
            || range.5 > 2
        {
            return Err(UnicodeDataError::Invalid(UnicodeDataKind::ScalarCoverage));
        }
        covered += (range.1 - range.0 + 1) as usize;
        next = if range.1 == 0xd7ff {
            0xe000
        } else {
            range.1 + 1
        };
    }
    if next != 0x110000 || covered != SCALAR_COUNT {
        return Err(UnicodeDataError::Invalid(UnicodeDataKind::IncompleteDomain));
    }
    let mut previous = None;
    for mapping in &data.case_mappings {
        if char::from_u32(mapping.0).is_none()
            || previous.is_some_and(|p| p >= mapping.0)
            || mapping.1.is_empty()
            || mapping.2.is_empty()
            || mapping.1.chars().count() > 3
            || mapping.2.chars().count() > 3
        {
            return Err(UnicodeDataError::Invalid(UnicodeDataKind::CaseMapping));
        }
        previous = Some(mapping.0);
    }
    if data.rich_narrow_to_wide.len() > 512
        || data
            .rich_narrow_to_wide
            .iter()
            .any(|&point| char::from_u32(point).is_none())
        || data
            .rich_narrow_to_wide
            .windows(2)
            .any(|pair| pair[0] >= pair[1])
    {
        return Err(UnicodeDataError::Invalid(UnicodeDataKind::RichVs16));
    }
    Ok(())
}

struct BoundedOutput {
    text: String,
    input_bytes: usize,
    nodes: usize,
    limits: UnicodeLimits,
}

impl BoundedOutput {
    fn new(limits: UnicodeLimits) -> Self {
        Self {
            text: String::new(),
            input_bytes: 0,
            nodes: 0,
            limits,
        }
    }

    fn input(&mut self, text: &str) -> Result<(), UnicodeError> {
        if text.len() > self.limits.input_bytes.saturating_sub(self.input_bytes) {
            return Err(UnicodeError::Limit(UnicodeResourceKind::InputBytes));
        }
        self.input_bytes += text.len();
        Ok(())
    }

    fn enter(&mut self, depth: usize) -> Result<(), UnicodeError> {
        if depth > self.limits.depth {
            return Err(UnicodeError::Limit(UnicodeResourceKind::Depth));
        }
        if self.nodes >= self.limits.nodes {
            return Err(UnicodeError::Limit(UnicodeResourceKind::Nodes));
        }
        self.nodes += 1;
        Ok(())
    }

    fn push(&mut self, text: &str) -> Result<(), UnicodeError> {
        if text.len() > self.limits.output_bytes.saturating_sub(self.text.len()) {
            return Err(UnicodeError::Limit(UnicodeResourceKind::OutputBytes));
        }
        self.text.push_str(text);
        Ok(())
    }

    fn character(&mut self, character: char) -> Result<(), UnicodeError> {
        self.push(character.encode_utf8(&mut [0; 4]))
    }
}

#[cfg(test)]
#[path = "../tests/production_unicode/mod.rs"]
mod tests;
