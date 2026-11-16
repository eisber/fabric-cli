// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use super::{MAX_MATRIX_CELLS, MAX_REQUEST_BYTES, MapsError, PROVIDER_ORIGIN};
use azure_core::http::{Method, Url};
use chrono::{DateTime, NaiveDate};
use serde_json::{Value, json};

/// A finite WGS84 position; GeoJSON always uses longitude, latitude.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Coordinate([f64; 2]);

impl Coordinate {
    /// Validate longitude [-180,180] and latitude [-90,90].
    pub fn new(longitude: f64, latitude: f64) -> Result<Self, MapsError> {
        if !longitude.is_finite()
            || !latitude.is_finite()
            || !(-180.0..=180.0).contains(&longitude)
            || !(-90.0..=90.0).contains(&latitude)
        {
            return Err(MapsError::InvalidInput);
        }
        Ok(Self([longitude, latitude]))
    }

    /// Return the original GeoJSON-ordered position without rounding.
    pub fn lon_lat(self) -> [f64; 2] {
        self.0
    }

    fn weather_query(self) -> String {
        format!("{},{}", self.0[1], self.0[0])
    }
}

/// Mutually exclusive free-text or structured geocoding input.
#[derive(Clone, Debug)]
pub enum Address {
    /// A nonempty address query, at most 512 UTF-8 bytes.
    Query(String),
    /// Exact provider address fields; no invented concatenation.
    Structured {
        /// Street address.
        address_line: String,
        /// City/locality.
        locality: String,
        /// State/administrative district.
        admin_district: String,
        /// Postal code.
        postal_code: String,
        /// Two-letter country/region.
        country_region: String,
    },
}

/// Provider weather units, preserved in result context and payload.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Units {
    /// Metric.
    Metric,
    /// Imperial.
    Imperial,
}

impl Units {
    /// Exact provider query value.
    pub fn name(self) -> &'static str {
        match self {
            Self::Metric => "metric",
            Self::Imperial => "imperial",
        }
    }
}

/// Explicit route context; directions and matrix have different traffic fields.
#[derive(Clone, Debug)]
pub struct RouteOptions {
    /// `driving`, `walking` or `truck`.
    pub mode: String,
    /// Directions: `none` or `live`; matrix: `historical` or `live`.
    pub traffic: String,
    /// RFC3339 with explicit offset; matrix additionally supports `any`/`now`.
    pub depart_at: Option<String>,
}

impl RouteOptions {
    fn validate(&self, matrix: bool) -> Result<(), MapsError> {
        if !matches!(self.mode.as_str(), "driving" | "walking" | "truck")
            || !(if matrix {
                matches!(self.traffic.as_str(), "historical" | "live")
            } else {
                matches!(self.traffic.as_str(), "none" | "live")
            })
        {
            return Err(MapsError::InvalidInput);
        }
        if let Some(departure) = &self.depart_at {
            let special = matrix && matches!(departure.as_str(), "any" | "now");
            if departure.len() > 40
                || (!special && DateTime::parse_from_rfc3339(departure).is_err())
                || (departure == "any" && self.traffic == "live")
            {
                return Err(MapsError::InvalidInput);
            }
        } else if matrix && self.traffic == "live" {
            // The matrix default is "any", incompatible with live traffic.
            return Err(MapsError::InvalidInput);
        }
        Ok(())
    }
}

/// One processing operation; builders enforce bounds before any effects.
#[derive(Clone, Debug)]
pub enum Operation {
    /// Preserve all candidates and confidence, including an empty result.
    Geocode {
        /// Free-text or structured address.
        address: Address,
        /// Maximum candidates, 1..=20.
        top: u8,
    },
    /// Reverse geocode one position.
    Reverse {
        /// Longitude, latitude.
        point: Coordinate,
    },
    /// Current conditions.
    CurrentWeather {
        /// Query location.
        point: Coordinate,
        /// Requested units.
        units: Units,
        /// Include detailed conditions.
        details: bool,
    },
    /// Daily forecast; day one is the provider's current forecast day.
    DailyForecast {
        /// Query location.
        point: Coordinate,
        /// Requested units.
        units: Units,
        /// One of 1, 5, 10 or 15 days.
        duration: u8,
    },
    /// Daily historical actuals, at most 31 inclusive calendar days.
    DailyHistoricalWeather {
        /// Query location.
        point: Coordinate,
        /// Requested units.
        units: Units,
        /// Inclusive YYYY-MM-DD.
        start_date: String,
        /// Inclusive YYYY-MM-DD.
        end_date: String,
    },
    /// One route, including full path and itinerary.
    Directions {
        /// Ordered waypoints, 2..=25.
        points: Vec<Coordinate>,
        /// Explicit travel/time/traffic context.
        options: RouteOptions,
    },
    /// Synchronous matrix, limited to 100 indexed origin/destination pairs.
    Matrix {
        /// Nonempty origin positions.
        origins: Vec<Coordinate>,
        /// Nonempty destination positions.
        destinations: Vec<Coordinate>,
        /// Explicit travel/time/traffic context.
        options: RouteOptions,
    },
}

/// Validated fixed-origin request; credentials are deliberately not represented.
#[derive(Clone, Debug)]
pub struct PreparedRequest {
    pub(super) operation: &'static str,
    pub(super) url: Url,
    pub(super) method: Method,
    pub(super) body: Vec<u8>,
    pub(super) context: Value,
    pub(super) dimensions: Option<(usize, usize)>,
}

impl PreparedRequest {
    /// Exact operation name.
    pub fn operation(&self) -> &'static str {
        self.operation
    }
    /// Fixed trusted provider URL, containing no credentials.
    pub fn url(&self) -> &Url {
        &self.url
    }
    /// GET or single-dispatch POST.
    pub fn method(&self) -> Method {
        self.method
    }
    /// GeoJSON request body, empty for GET.
    pub fn body(&self) -> &[u8] {
        &self.body
    }
    /// Explicit semantic input context, not an invocation-summary resultset.
    pub fn context(&self) -> &Value {
        &self.context
    }
}

fn text(value: &str) -> Result<(), MapsError> {
    if value.trim().is_empty() || value.len() > 512 || value.chars().any(char::is_control) {
        return Err(MapsError::InvalidInput);
    }
    Ok(())
}

fn date(value: &str) -> Result<NaiveDate, MapsError> {
    if value.len() != 10
        || !value.bytes().enumerate().all(|(index, byte)| {
            if [4, 7].contains(&index) {
                byte == b'-'
            } else {
                byte.is_ascii_digit()
            }
        })
        || value.starts_with("0000")
    {
        return Err(MapsError::InvalidInput);
    }
    NaiveDate::parse_from_str(value, "%Y-%m-%d").map_err(|_| MapsError::InvalidInput)
}

impl Operation {
    /// Validate the complete operation and construct its supported provider shape.
    pub fn prepare(&self) -> Result<PreparedRequest, MapsError> {
        let mut query: Vec<(&str, String)> = Vec::new();
        let mut body = Value::Null;
        let mut dimensions = None;
        let (operation, path, version, context) = match self {
            Self::Geocode { address, top } => {
                if !(1..=20).contains(top) {
                    return Err(MapsError::InvalidInput);
                }
                match address {
                    Address::Query(value) => {
                        text(value)?;
                        query.push(("query", value.clone()));
                    }
                    Address::Structured {
                        address_line,
                        locality,
                        admin_district,
                        postal_code,
                        country_region,
                    } => {
                        for (name, value) in [
                            ("addressLine", address_line),
                            ("locality", locality),
                            ("adminDistrict", admin_district),
                            ("postalCode", postal_code),
                            ("countryRegion", country_region),
                        ] {
                            text(value)?;
                            query.push((name, value.clone()));
                        }
                        if country_region.len() != 2
                            || !country_region.bytes().all(|c| c.is_ascii_alphabetic())
                        {
                            return Err(MapsError::InvalidInput);
                        }
                    }
                }
                query.push(("top", top.to_string()));
                (
                    "geocode",
                    "/geocode",
                    "2025-01-01",
                    json!({"top":top,"coordinateOrder":"longitude,latitude","qualityGuarantee":false}),
                )
            }
            Self::Reverse { point } => {
                let [lon, lat] = point.0;
                query.push(("coordinates", format!("{lon},{lat}")));
                (
                    "reverse",
                    "/reverseGeocode",
                    "2025-01-01",
                    json!({"coordinates":point.0,"coordinateOrder":"longitude,latitude","qualityGuarantee":false}),
                )
            }
            Self::CurrentWeather {
                point,
                units,
                details,
            } => {
                query.extend([
                    ("query", point.weather_query()),
                    ("unit", units.name().into()),
                    ("details", details.to_string()),
                ]);
                (
                    "current",
                    "/weather/currentConditions/json",
                    "1.1",
                    json!({"coordinates":point.0,"weatherQueryOrder":"latitude,longitude","units":units.name(),"details":details}),
                )
            }
            Self::DailyForecast {
                point,
                units,
                duration,
            } => {
                if ![1, 5, 10, 15].contains(duration) {
                    return Err(MapsError::InvalidInput);
                }
                query.extend([
                    ("query", point.weather_query()),
                    ("unit", units.name().into()),
                    ("duration", duration.to_string()),
                ]);
                (
                    "dailyforecast",
                    "/weather/forecast/daily/json",
                    "1.1",
                    json!({"coordinates":point.0,"weatherQueryOrder":"latitude,longitude","units":units.name(),"duration":duration,"firstDay":"provider-current-forecast-day"}),
                )
            }
            Self::DailyHistoricalWeather {
                point,
                units,
                start_date,
                end_date,
            } => {
                let days = date(end_date)?
                    .signed_duration_since(date(start_date)?)
                    .num_days();
                if !(0..31).contains(&days) {
                    return Err(MapsError::InvalidInput);
                }
                query.extend([
                    ("query", point.weather_query()),
                    ("unit", units.name().into()),
                    ("startDate", start_date.clone()),
                    ("endDate", end_date.clone()),
                ]);
                (
                    "dailyhistoricalweather",
                    "/weather/historical/actuals/daily/json",
                    "1.1",
                    json!({"coordinates":point.0,"weatherQueryOrder":"latitude,longitude","units":units.name(),"startDate":start_date,"endDate":end_date}),
                )
            }
            Self::Directions { points, options } => {
                options.validate(false)?;
                if !(2..=25).contains(&points.len()) {
                    return Err(MapsError::InvalidInput);
                }
                body = json!({
                    "type":"FeatureCollection",
                    "features":points.iter().enumerate().map(|(index, point)| json!({
                        "type":"Feature","geometry":{"type":"Point","coordinates":point.0},
                        "properties":{"pointIndex":index,"pointType":"waypoint"}
                    })).collect::<Vec<_>>(),
                    "travelMode":options.mode,"maxRouteCount":1,
                    "routeOutputOptions":["routePath","itinerary"],
                    "optimizeRoute":if options.traffic == "live" {"fastestWithTraffic"} else {"fastestWithoutTraffic"}
                });
                if let Some(depart_at) = &options.depart_at {
                    body["departAt"] = json!(depart_at);
                }
                (
                    "directions",
                    "/route/directions",
                    "2025-01-01",
                    json!({"travelMode":options.mode,"traffic":options.traffic,"departAt":options.depart_at,"departureDefault":"provider-current-time","coordinateOrder":"longitude,latitude"}),
                )
            }
            Self::Matrix {
                origins,
                destinations,
                options,
            } => {
                options.validate(true)?;
                let cells = origins
                    .len()
                    .checked_mul(destinations.len())
                    .ok_or(MapsError::InvalidInput)?;
                if cells == 0 || cells > MAX_MATRIX_CELLS {
                    return Err(MapsError::InvalidInput);
                }
                dimensions = Some((origins.len(), destinations.len()));
                body = json!({
                    "type":"FeatureCollection",
                    "features":[
                        {"type":"Feature","geometry":{"type":"MultiPoint","coordinates":origins.iter().map(|p| p.0).collect::<Vec<_>>()},"properties":{"pointType":"origins"}},
                        {"type":"Feature","geometry":{"type":"MultiPoint","coordinates":destinations.iter().map(|p| p.0).collect::<Vec<_>>()},"properties":{"pointType":"destinations"}}
                    ],
                    "travelMode":options.mode,"traffic":options.traffic,
                    "departAt":options.depart_at.as_deref().unwrap_or("any")
                });
                (
                    "matrix",
                    "/route/matrix",
                    "2025-01-01",
                    json!({"travelMode":options.mode,"traffic":options.traffic,"departAt":options.depart_at.as_deref().unwrap_or("any"),"origins":origins.len(),"destinations":destinations.len(),"coordinateOrder":"longitude,latitude"}),
                )
            }
        };
        let mut url =
            Url::parse(&format!("{PROVIDER_ORIGIN}{path}")).map_err(|_| MapsError::InvalidInput)?;
        url.query_pairs_mut()
            .append_pair("api-version", version)
            .extend_pairs(query);
        let has_body = !body.is_null();
        let body = if has_body {
            serde_json::to_vec(&body).map_err(|_| MapsError::InvalidInput)?
        } else {
            Vec::new()
        };
        if body.len() > MAX_REQUEST_BYTES || url.as_str().len() > 8 * 1024 {
            return Err(MapsError::InvalidInput);
        }
        Ok(PreparedRequest {
            operation,
            url,
            method: if has_body { Method::Post } else { Method::Get },
            body,
            context,
            dimensions,
        })
    }
}
