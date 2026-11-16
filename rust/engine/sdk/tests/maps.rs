// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Synthetic contract fixtures; not captured native or live provider payloads.
#![deny(unnameable_test_items)]

use azure_core::http::Method;
use fabric_sdk::maps::{
    Address, Coordinate, MapsError, Operation, ProcessingResult, RouteOptions, Units, shared_issuer,
};
use serde_json::{Value, json};

fn point() -> Coordinate {
    Coordinate::new(-73.9632, 40.7794).unwrap()
}
fn route(matrix: bool) -> RouteOptions {
    RouteOptions {
        mode: "driving".into(),
        traffic: if matrix { "historical" } else { "none" }.into(),
        depart_at: None,
    }
}
fn matrix() -> Operation {
    Operation::Matrix {
        origins: vec![point(), point()],
        destinations: vec![point(), point()],
        options: route(true),
    }
}
fn cell(origin: usize, destination: usize, status: u16) -> Value {
    json!({"originIndex":origin,"destinationIndex":destination,"statusCode":status,"distanceInMeters":933,"durationInSeconds":271,"arrivalAt":"2026-11-15T17:47:00-05:00"})
}
fn result(operation: Operation, payload: Value) -> Result<ProcessingResult, MapsError> {
    ProcessingResult::parse(
        &operation.prepare().unwrap(),
        &serde_json::to_vec(&payload).unwrap(),
    )
}

#[test]
fn coordinate_order_and_all_seven_exact_paths_versions_and_methods() {
    let operations = [
        (
            Operation::Geocode {
                address: Address::Query("1000 Fifth Avenue, New York, NY".into()),
                top: 1,
            },
            "/geocode",
            "2025-01-01",
            Method::Get,
        ),
        (
            Operation::Reverse { point: point() },
            "/reverseGeocode",
            "2025-01-01",
            Method::Get,
        ),
        (
            Operation::CurrentWeather {
                point: point(),
                units: Units::Metric,
                details: false,
            },
            "/weather/currentConditions/json",
            "1.1",
            Method::Get,
        ),
        (
            Operation::DailyForecast {
                point: point(),
                units: Units::Imperial,
                duration: 1,
            },
            "/weather/forecast/daily/json",
            "1.1",
            Method::Get,
        ),
        (
            Operation::DailyHistoricalWeather {
                point: point(),
                units: Units::Metric,
                start_date: "2026-11-13".into(),
                end_date: "2026-11-13".into(),
            },
            "/weather/historical/actuals/daily/json",
            "1.1",
            Method::Get,
        ),
        (
            Operation::Directions {
                points: vec![point(), point()],
                options: route(false),
            },
            "/route/directions",
            "2025-01-01",
            Method::Post,
        ),
        (matrix(), "/route/matrix", "2025-01-01", Method::Post),
    ];
    for (operation, path, version, method) in operations {
        let prepared = operation.prepare().unwrap();
        assert_eq!(
            prepared.url().origin().ascii_serialization(),
            "https://atlas.microsoft.com"
        );
        assert_eq!(prepared.url().path(), path);
        assert!(
            prepared
                .url()
                .query_pairs()
                .any(|(k, v)| k == "api-version" && v == version)
        );
        assert_eq!(prepared.method(), method);
        let query = prepared
            .url()
            .query_pairs()
            .collect::<std::collections::BTreeMap<_, _>>();
        if path.starts_with("/weather/") {
            assert_eq!(query["query"], "40.7794,-73.9632");
        }
        if path == "/reverseGeocode" {
            assert_eq!(query["coordinates"], "-73.9632,40.7794");
        }
        if method == Method::Post {
            let body: Value = serde_json::from_slice(prepared.body()).unwrap();
            assert_eq!(body["type"], "FeatureCollection");
            let coordinates = &body["features"][0]["geometry"]["coordinates"];
            assert_eq!(
                if path.ends_with("matrix") {
                    &coordinates[0]
                } else {
                    coordinates
                },
                &json!([-73.9632, 40.7794])
            );
        }
    }
}

#[test]
fn structured_geocode_preserves_exact_fields_and_never_promises_precision() {
    let request = Operation::Geocode {
        address: Address::Structured {
            address_line: "1000 5th Ave".into(),
            locality: "New York".into(),
            admin_district: "NY".into(),
            postal_code: "10028".into(),
            country_region: "US".into(),
        },
        top: 1,
    }
    .prepare()
    .unwrap();
    let query = request
        .url()
        .query_pairs()
        .collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(query["addressLine"], "1000 5th Ave");
    assert!(!query.contains_key("query"));
    let payload = json!({"type":"FeatureCollection","features":[
        {"type":"Feature","geometry":{"type":"Point","coordinates":[-73.96016896337983,40.781764687428804]},"properties":{"confidence":"High","distanceFromReferenceMeters":366,"within250m":false,"candidates":[null,{"match":"street"}]}}
    ]});
    let result = ProcessingResult::parse(&request, &serde_json::to_vec(&payload).unwrap()).unwrap();
    assert_eq!(result.document()["data"], payload);
    assert_eq!(result.document()["context"]["qualityGuarantee"], false);
}

#[test]
fn strict_input_budgets_reject_before_execution() {
    for (lon, lat) in [
        (f64::NAN, 0.0),
        (0.0, f64::INFINITY),
        (180.1, 0.0),
        (0.0, -90.1),
    ] {
        assert!(Coordinate::new(lon, lat).is_err());
    }
    for top in [0, 21, 255] {
        assert!(
            Operation::Geocode {
                address: Address::Query("test".into()),
                top
            }
            .prepare()
            .is_err()
        );
    }
    for query in ["".into(), "x".repeat(513), "line\nbreak".into()] {
        assert!(
            Operation::Geocode {
                address: Address::Query(query),
                top: 1
            }
            .prepare()
            .is_err()
        );
    }
    for duration in [0, 2, 16, 255] {
        assert!(
            Operation::DailyForecast {
                point: point(),
                units: Units::Metric,
                duration
            }
            .prepare()
            .is_err()
        );
    }
    for (start, end) in [
        ("2026-02-29", "2026-03-01"),
        ("2026-11-15", "2026-11-14"),
        ("2026-01-01", "2026-02-01"),
        ("2026-1-01", "2026-01-01"),
    ] {
        assert!(
            Operation::DailyHistoricalWeather {
                point: point(),
                units: Units::Metric,
                start_date: start.into(),
                end_date: end.into()
            }
            .prepare()
            .is_err()
        );
    }
    assert!(
        Operation::DailyHistoricalWeather {
            point: point(),
            units: Units::Metric,
            start_date: "2024-02-29".into(),
            end_date: "2024-03-30".into()
        }
        .prepare()
        .is_ok()
    );
    assert!(
        Operation::Matrix {
            origins: vec![point(); 10],
            destinations: vec![point(); 10],
            options: route(true)
        }
        .prepare()
        .is_ok()
    );
    assert!(
        Operation::Matrix {
            origins: vec![point(); 101],
            destinations: vec![point()],
            options: route(true)
        }
        .prepare()
        .is_err()
    );
    assert!(
        Operation::Matrix {
            origins: vec![],
            destinations: vec![point()],
            options: route(true)
        }
        .prepare()
        .is_err()
    );
    for count in [0, 1, 26] {
        assert!(
            Operation::Directions {
                points: vec![point(); count],
                options: route(false)
            }
            .prepare()
            .is_err()
        );
    }
}

#[test]
fn traffic_and_offset_semantics_are_explicit_and_version_correct() {
    let options = RouteOptions {
        mode: "driving".into(),
        traffic: "live".into(),
        depart_at: Some("2026-11-15T17:47:00-05:00".into()),
    };
    for operation in [
        Operation::Directions {
            points: vec![point(); 2],
            options: options.clone(),
        },
        Operation::Matrix {
            origins: vec![point()],
            destinations: vec![point()],
            options,
        },
    ] {
        let request = operation.prepare().unwrap();
        let body: Value = serde_json::from_slice(request.body()).unwrap();
        assert_eq!(body["departAt"], "2026-11-15T17:47:00-05:00");
        if request.operation() == "directions" {
            assert_eq!(body["optimizeRoute"], "fastestWithTraffic");
            assert!(body.get("traffic").is_none());
            assert_eq!(body["features"][1]["properties"]["pointIndex"], 1);
            assert_eq!(
                body["routeOutputOptions"],
                json!(["routePath", "itinerary"])
            );
        } else {
            assert_eq!(body["traffic"], "live");
        }
    }
    for departure in [
        None,
        Some("any"),
        Some("2026-11-15T17:47:00"),
        Some("not-time"),
    ] {
        assert!(
            Operation::Matrix {
                origins: vec![point()],
                destinations: vec![point()],
                options: RouteOptions {
                    mode: "driving".into(),
                    traffic: "live".into(),
                    depart_at: departure.map(str::to_owned)
                }
            }
            .prepare()
            .is_err()
        );
    }
}

#[test]
fn trusted_shared_origin_validation_rejects_credential_forwarding_tricks() {
    assert_eq!(
        shared_issuer("https://df-msit-scus-redirect.analysis.windows.net")
            .unwrap()
            .as_str(),
        "https://df-msit-scus-redirect.analysis.windows.net/metadata/v202212/azureMapToken"
    );
    for origin in [
        "http://wabi.analysis.windows.net",
        "https://analysis.windows.net",
        "https://analysis.windows.net.attacker.test",
        "https://attacker.test",
        "https://user@wabi.analysis.windows.net",
        "https://wabi.analysis.windows.net:444",
        "https://wabi.analysis.windows.net/path",
        "https://wabi.analysis.windows.net?token=x",
        "https://wabi.analysis.windows.net/#fragment",
        "https://127.0.0.1",
    ] {
        assert!(shared_issuer(origin).is_err(), "{origin}");
    }
}

#[test]
fn weather_keeps_offsets_units_nulls_and_forecast_day() {
    let forecast = json!({"summary":{"phrase":"Synthetic only"},"forecasts":[
        {"date":"2026-11-15T07:00:00-05:00","temperature":{"minimum":{"value":null,"unit":"C","unitType":17}},"day":{"phrase":"Synthetic"},"night":null,"sources":["synthetic"]}
    ]});
    let result = result(
        Operation::DailyForecast {
            point: point(),
            units: Units::Metric,
            duration: 1,
        },
        forecast.clone(),
    )
    .unwrap();
    assert_eq!(result.document()["data"], forecast);
    assert_eq!(
        result.document()["context"]["firstDay"],
        "provider-current-forecast-day"
    );
    assert_eq!(
        result.export_rows().unwrap()[0]["responseMetadata"]["summary"],
        forecast["summary"]
    );
    assert!(result.complete());
    assert!(
        !self::result(
            Operation::CurrentWeather {
                point: point(),
                units: Units::Metric,
                details: false
            },
            json!({"results":[null]})
        )
        .unwrap()
        .complete()
    );
}

#[test]
fn matrix_accounts_every_expected_pair_and_partial_status_without_outer200_shortcut() {
    let payload = json!({"type":"Feature","geometry":null,"properties":{"matrix":[cell(1,1,429),cell(0,0,200),cell(1,0,200)],"summary":{"totalCount":4,"successfulCount":2}}});
    let result = result(matrix(), payload.clone()).unwrap();
    assert!(!result.complete());
    assert_eq!(result.document()["data"], payload);
    assert_eq!(
        result.document()["matrixCells"].as_array().unwrap().len(),
        4
    );
    let rows = result.export_rows().unwrap();
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[1]["originIndex"], 0);
    assert_eq!(rows[1]["destinationIndex"], 1);
    assert!(rows[1]["statusCode"].is_null());
    assert!(rows[1]["result"].is_null());
    assert_eq!(rows[3]["statusCode"], 429);
}

#[test]
fn malformed_matrix_and_nonresult_shapes_fail_explicitly() {
    for cells in [
        json!([cell(0, 0, 200), cell(0, 0, 200)]),
        json!([cell(2, 0, 200)]),
        json!([{"originIndex":0,"destinationIndex":0,"statusCode":200}]),
        json!([{"originIndex":0,"destinationIndex":0,"statusCode":0}]),
        json!([{"originIndex":-1,"destinationIndex":0,"statusCode":200}]),
    ] {
        assert!(
            result(
                matrix(),
                json!({"type":"Feature","properties":{"matrix":cells}})
            )
            .is_err()
        );
    }
    for payload in [
        json!({"error":{"message":"secret"}}),
        json!({}),
        json!(null),
        json!({"results":"not-array"}),
    ] {
        assert!(
            result(
                Operation::CurrentWeather {
                    point: point(),
                    units: Units::Metric,
                    details: false
                },
                payload
            )
            .is_err()
        );
    }
}

#[test]
fn full_synthetic_route_and_historical_payloads_keep_geometry_temporal_and_null_fields() {
    let payload = json!({
        "type":"FeatureCollection","bbox":[-74.0,40.0,-73.0,41.0],
        "features":[{"type":"Feature","geometry":{"type":"MultiLineString","coordinates":[[[-73.9632,40.7794],[-73.959,40.783]]]},"properties":{"type":"RoutePath","distanceInMeters":933,"durationInSeconds":169,"departureAt":"2026-11-15T17:47:00-05:00","legs":[{"itinerary":null}]}}],
        "alternativeRoutes":[]
    });
    let result = result(
        Operation::Directions {
            points: vec![point(); 2],
            options: route(false),
        },
        payload.clone(),
    )
    .unwrap();
    assert!(result.complete());
    assert_eq!(result.document()["data"], payload);
    assert_eq!(
        result.export_rows().unwrap()[0]["responseMetadata"]["bbox"],
        payload["bbox"]
    );
    let history = json!({"results":[{"date":"2026-11-13T00:00:00-05:00","temperature":{"minimum":{"value":null,"unit":"C"}},"precipitation":{"value":0,"unit":"mm"},"snowfall":null,"snowDepth":null}]});
    let result = self::result(
        Operation::DailyHistoricalWeather {
            point: point(),
            units: Units::Metric,
            start_date: "2026-11-13".into(),
            end_date: "2026-11-13".into(),
        },
        history.clone(),
    )
    .unwrap();
    assert!(result.complete());
    assert_eq!(result.document()["data"], history);
}

#[test]
fn malformed_geometry_and_expanded_metadata_budget_fail_before_output_allocation() {
    for geometry in [
        json!({"type":"Point","coordinates":[0,91]}),
        json!({"type":"Point","coordinates":[]}),
        json!({}),
    ] {
        assert!(result(Operation::Reverse {point:point()},json!({"type":"FeatureCollection","features":[{"type":"Feature","geometry":geometry}]})).is_err());
    }
    let request = Operation::CurrentWeather {
        point: point(),
        units: Units::Metric,
        details: false,
    }
    .prepare()
    .unwrap();
    let payload = json!({"largeMetadata":"x".repeat(900_000),"results":vec![json!({"dateTime":"2026-11-15T17:47:00-05:00"});10]});
    let result = ProcessingResult::parse(&request, &serde_json::to_vec(&payload).unwrap()).unwrap();
    assert!(result.export_rows().is_err());
}
