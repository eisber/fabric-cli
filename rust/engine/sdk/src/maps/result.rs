// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{Coordinate, MAX_OUTPUT_BYTES, MAX_RESPONSE_BYTES, MapsError, PreparedRequest};
use serde::Serialize;
use serde_json::{Value, json};

/// Accounting for one expected matrix pair, including missing provider cells.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MatrixCell {
    /// Zero-based origin index.
    pub origin_index: usize,
    /// Zero-based destination index.
    pub destination_index: usize,
    /// Provider status, or null when the expected cell is missing.
    pub status_code: Option<u16>,
}

/// Full provider data and separate semantic/partial-result evidence.
#[derive(Clone, Debug)]
pub struct ProcessingResult {
    document: Value,
    complete: bool,
    rows: Vec<Value>,
}

impl ProcessingResult {
    /// Validate a complete bounded HTTP 200 payload without flattening its data.
    ///
    /// Missing matrix pairs become explicit null-status cells and nonzero CLI
    /// outcomes. Duplicate/out-of-range indices are malformed, never discarded.
    pub fn parse(request: &PreparedRequest, bytes: &[u8]) -> Result<Self, MapsError> {
        if bytes.len() > MAX_RESPONSE_BYTES {
            return Err(MapsError::InvalidResponse);
        }
        let payload: Value =
            serde_json::from_slice(bytes).map_err(|_| MapsError::InvalidResponse)?;
        if !payload.is_object() || payload.get("error").is_some() {
            return Err(MapsError::InvalidResponse);
        }
        let mut complete = true;
        let mut accounting = Vec::new();
        let rows = if let Some((origins, destinations)) = request.dimensions {
            if payload["type"] != "Feature" {
                return Err(MapsError::InvalidResponse);
            }
            let matrix = payload
                .pointer("/properties/matrix")
                .and_then(Value::as_array)
                .ok_or(MapsError::InvalidResponse)?;
            let mut cells: Vec<Option<&Value>> = vec![None; origins * destinations];
            for cell in matrix {
                let origin = cell["originIndex"]
                    .as_u64()
                    .and_then(|v| usize::try_from(v).ok())
                    .ok_or(MapsError::InvalidResponse)?;
                let destination = cell["destinationIndex"]
                    .as_u64()
                    .and_then(|v| usize::try_from(v).ok())
                    .ok_or(MapsError::InvalidResponse)?;
                let status = cell["statusCode"]
                    .as_u64()
                    .filter(|v| (100..=599).contains(v))
                    .ok_or(MapsError::InvalidResponse)?;
                if origin >= origins
                    || destination >= destinations
                    || cells[origin * destinations + destination].is_some()
                {
                    return Err(MapsError::InvalidResponse);
                }
                if status == 200 {
                    for field in ["distanceInMeters", "durationInSeconds"] {
                        if cell[field]
                            .as_f64()
                            .filter(|v| v.is_finite() && *v >= 0.0)
                            .is_none()
                        {
                            return Err(MapsError::InvalidResponse);
                        }
                    }
                }
                cells[origin * destinations + destination] = Some(cell);
            }
            cells.into_iter().enumerate().map(|(index, cell)| {
                let status = cell.and_then(|v| v["statusCode"].as_u64()).map(|v| v as u16);
                complete &= status == Some(200);
                accounting.push(MatrixCell { origin_index: index / destinations, destination_index: index % destinations, status_code: status });
                json!({"originIndex":index / destinations,"destinationIndex":index % destinations,"statusCode":status,"result":cell})
            }).collect::<Vec<_>>()
        } else {
            let field = match request.operation {
                "geocode" | "reverse" | "directions" => {
                    if payload["type"] != "FeatureCollection" {
                        return Err(MapsError::InvalidResponse);
                    }
                    "features"
                }
                "dailyforecast" => "forecasts",
                _ => "results",
            };
            let values = payload
                .get(field)
                .and_then(Value::as_array)
                .ok_or(MapsError::InvalidResponse)?;
            if values.len() > 10_000 || values.iter().any(|v| !v.is_null() && !v.is_object()) {
                return Err(MapsError::InvalidResponse);
            }
            // Null candidates/records remain visible, rather than filtering rows.
            if values.iter().any(Value::is_null) {
                complete = false;
            }
            match request.operation {
                "current" | "dailyforecast" | "dailyhistoricalweather" => {
                    let timestamp = if request.operation == "current" {
                        "dateTime"
                    } else {
                        "date"
                    };
                    complete &= values.iter().all(|value| {
                        value[timestamp]
                            .as_str()
                            .is_some_and(|date| chrono::DateTime::parse_from_rfc3339(date).is_ok())
                    });
                }
                "geocode" | "reverse" => {
                    for value in values {
                        if value["geometry"].is_null() {
                            complete = false;
                            continue;
                        }
                        if value["geometry"]["type"] != "Point" {
                            return Err(MapsError::InvalidResponse);
                        }
                        validate_position(&value["geometry"]["coordinates"])?;
                    }
                }
                "directions" => {
                    complete &= values.iter().any(|value| {
                        value.pointer("/geometry/type").and_then(Value::as_str)
                            == Some("MultiLineString")
                            && value
                                .pointer("/geometry/coordinates")
                                .and_then(Value::as_array)
                                .is_some_and(|lines| !lines.is_empty())
                    });
                    for value in values {
                        if value.pointer("/geometry/type").and_then(Value::as_str)
                            == Some("MultiLineString")
                        {
                            let lines = value["geometry"]["coordinates"]
                                .as_array()
                                .filter(|lines| !lines.is_empty())
                                .ok_or(MapsError::InvalidResponse)?;
                            for line in lines {
                                let points = line
                                    .as_array()
                                    .filter(|points| points.len() >= 2)
                                    .ok_or(MapsError::InvalidResponse)?;
                                for point in points {
                                    validate_position(point)?;
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
            values
                .iter()
                .enumerate()
                .map(|(index, value)| json!({"resultIndex":index,"result":value}))
                .collect()
        };
        let document = json!({
            "operation":request.operation,
            "provider":"AzureMaps",
            "apiVersion":request.url.query_pairs().find(|(k,_)| k == "api-version").map(|(_,v)|v.into_owned()),
            "contract":"experimental-private-Shared-SAS",
            "context":request.context,
            "complete":complete,
            "resultCount":rows.len(),
            "matrixCells":accounting,
            "data":payload
        });
        Ok(Self {
            document,
            complete,
            rows,
        })
    }

    /// Full fidelity data envelope, including confidence, geometry and time offsets.
    pub fn document(&self) -> &Value {
        &self.document
    }
    /// Whether all expected records/cells are present and successful.
    pub fn complete(&self) -> bool {
        self.complete
    }
    /// Real result rows, never invocation-summary exports.
    ///
    /// Each row keeps the entire nested result as logical JSON. Provenance and
    /// context are repeated so a standalone table does not lose those semantics.
    pub fn export_rows(&self) -> Result<Vec<Value>, MapsError> {
        let mut response_metadata = self.document["data"].clone();
        if let Some(fields) = response_metadata.as_object_mut() {
            let collection = match self.document["operation"].as_str() {
                Some("geocode" | "reverse" | "directions") => "features",
                Some("dailyforecast") => "forecasts",
                Some("matrix") => "properties",
                _ => "results",
            };
            if collection == "properties" {
                if let Some(properties) =
                    fields.get_mut("properties").and_then(Value::as_object_mut)
                {
                    properties.remove("matrix");
                }
            } else {
                fields.remove(collection);
            }
        }
        let common_bytes = serde_json::to_vec(&response_metadata)
            .and_then(|metadata| {
                serde_json::to_vec(&self.document["context"])
                    .map(|context| metadata.len() + context.len() + 1024)
            })
            .map_err(|_| MapsError::InvalidResponse)?;
        let mut size = common_bytes
            .checked_mul(self.rows.len())
            .ok_or(MapsError::InvalidResponse)?;
        for row in &self.rows {
            size = size
                .checked_add(
                    serde_json::to_vec(row)
                        .map_err(|_| MapsError::InvalidResponse)?
                        .len(),
                )
                .filter(|size| *size <= MAX_OUTPUT_BYTES)
                .ok_or(MapsError::InvalidResponse)?;
        }
        if size > MAX_OUTPUT_BYTES {
            return Err(MapsError::InvalidResponse);
        }
        Ok(self
            .rows
            .iter()
            .map(|row| {
                let mut row = row.clone();
                row["operation"] = self.document["operation"].clone();
                row["provider"] = self.document["provider"].clone();
                row["apiVersion"] = self.document["apiVersion"].clone();
                row["contract"] = self.document["contract"].clone();
                row["context"] = self.document["context"].clone();
                row["responseMetadata"] = response_metadata.clone();
                row["complete"] = json!(self.complete);
                row
            })
            .collect())
    }
}

fn validate_position(position: &Value) -> Result<(), MapsError> {
    let values = position
        .as_array()
        .filter(|values| values.len() == 2)
        .ok_or(MapsError::InvalidResponse)?;
    Coordinate::new(
        values[0].as_f64().ok_or(MapsError::InvalidResponse)?,
        values[1].as_f64().ok_or(MapsError::InvalidResponse)?,
    )
    .map(|_| ())
    .map_err(|_| MapsError::InvalidResponse)
}
