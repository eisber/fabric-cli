// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

use clap::{Arg, ArgAction, ArgMatches, Command, error::ErrorKind};
use fabric_sdk::maps::{Address, Coordinate, Operation, PreparedRequest, RouteOptions, Units};
use std::{ffi::OsString, path::PathBuf, time::Duration};

/// Validated input; construction performs no file, credential or network access.
#[derive(Clone, Debug)]
pub struct MapsCommand {
    /// Complete validated provider request.
    pub request: PreparedRequest,
    /// One absolute budget covering acquisition, discovery, issuance and processing.
    pub timeout: Duration,
    /// Optional local synthetic provider-payload replay; never acquires credentials.
    pub offline_fixture: Option<PathBuf>,
    /// Optional data projection; never changes partial-failure exit status.
    pub query: Option<String>,
    /// Terminal `json` or `text`.
    pub output_format: String,
    /// Explicit complete result-data file delivery.
    pub export: Option<crate::tabular_export::ExportRequest>,
}

fn option(name: &'static str) -> Arg {
    Arg::new(name).long(name).num_args(1)
}

fn point_arg(name: &'static str) -> Arg {
    option(name).allow_hyphen_values(true)
}

fn weather(command: Command) -> Command {
    command
        .arg(
            point_arg("coordinates")
                .required(true)
                .help("WGS84 longitude,latitude (weather query is reordered internally)"),
        )
        .arg(
            option("units")
                .value_parser(["metric", "imperial"])
                .default_value("metric"),
        )
}

fn route(command: Command, matrix: bool) -> Command {
    command
        .arg(
            option("mode")
                .value_parser(["driving", "walking", "truck"])
                .default_value("driving"),
        )
        .arg(if matrix {
            option("traffic")
                .value_parser(["historical", "live"])
                .default_value("historical")
        } else {
            option("traffic")
                .value_parser(["none", "live"])
                .default_value("none")
        })
        .arg(
            option("depart-at")
                .help("RFC3339 with explicit offset; matrix also accepts any or now"),
        )
}

/// Build offline help without discovering identity, configuration or services.
pub fn maps_command() -> Command {
    Command::new("maps")
        .about("Experimental Maps processing; private Shared SAS contract, not public Fabric v1")
        .after_help("Online execution requires an existing native broker identity and applicable private Shared client admission and entitlement; no experimental opt-in flag is required. App-only/device-code admission, billing and redistribution terms are unqualified. --offline-fixture reads synthetic local JSON only; it is not native/live qualification. No tileset, saved Map renderer or PNG support.")
        .subcommand_required(true)
        .arg(option("timeout").global(true).default_value("60").help("Total deadline in seconds, 1..=120"))
        .arg(option("offline-fixture").global(true).help("Offline-only synthetic provider JSON; never contacts any service"))
        .arg(option("query").global(true).help("Bounded JMESPath field/index/wildcard path only; no functions, filters, pipes or constructors"))
        .arg(option("output-format").global(true).value_parser(["json", "text"]).default_value("json"))
        .arg(option("format").global(true).value_parser(["json", "csv", "parquet"]).requires("output").help("Export real result rows with logical JSON cells, not invocation summaries"))
        .arg(option("output").global(true).requires("format"))
        .subcommand(Command::new("geocode")
            .arg(option("address").conflicts_with_all(["address-line", "locality", "admin-district", "postal-code", "country-region"]))
            .arg(option("address-line").requires_all(["locality", "admin-district", "postal-code", "country-region"]))
            .arg(option("locality").requires("address-line"))
            .arg(option("admin-district").requires("address-line"))
            .arg(option("postal-code").requires("address-line"))
            .arg(option("country-region").requires("address-line"))
            .arg(option("top").default_value("5")))
        .subcommand(Command::new("reverse").arg(point_arg("coordinates").required(true)))
        .subcommand(weather(Command::new("current")).arg(Arg::new("details").long("details").action(ArgAction::SetTrue)))
        .subcommand(weather(Command::new("dailyforecast")).arg(option("duration").default_value("1")))
        .subcommand(weather(Command::new("dailyhistoricalweather"))
            .arg(option("start-date").required(true)).arg(option("end-date").required(true)))
        .subcommand(route(Command::new("directions"), false)
            .arg(point_arg("point").required(true).action(ArgAction::Append).help("Repeat 2..=25 times in waypoint order")))
        .subcommand(route(Command::new("matrix"), true)
            .arg(point_arg("origin").required(true).action(ArgAction::Append))
            .arg(point_arg("destination").required(true).action(ArgAction::Append)))
}

fn invalid() -> clap::Error {
    maps_command().error(
        ErrorKind::ValueValidation,
        "Invalid or excessive Maps input; see fab-rs item maps <operation> --help.",
    )
}

fn value<'a>(args: &'a ArgMatches, name: &str) -> Result<&'a str, clap::Error> {
    args.get_one::<String>(name)
        .map(String::as_str)
        .ok_or_else(invalid)
}

fn coordinate(value: &str) -> Result<Coordinate, clap::Error> {
    if value.len() > 80 {
        return Err(invalid());
    }
    let (longitude, latitude) = value.split_once(',').ok_or_else(invalid)?;
    Coordinate::new(
        longitude.parse().map_err(|_| invalid())?,
        latitude.parse().map_err(|_| invalid())?,
    )
    .map_err(|_| invalid())
}

fn points(args: &ArgMatches, name: &str) -> Result<Vec<Coordinate>, clap::Error> {
    let values = args.get_many::<String>(name).ok_or_else(invalid)?;
    if values.len() > 100 {
        return Err(invalid());
    }
    values.map(|v| coordinate(v)).collect()
}

/// Parse and validate every option before credentials, filesystem or networking.
pub fn parse(arguments: impl IntoIterator<Item = OsString>) -> Result<MapsCommand, clap::Error> {
    let arguments: Vec<_> = arguments.into_iter().take(301).collect();
    validate_arguments(&arguments, 300)?;
    let matches = maps_command()
        .bin_name("fab-rs item maps")
        .try_get_matches_from(std::iter::once(OsString::from("maps")).chain(arguments))?;
    from_matches(&matches)
}

pub(crate) fn validate_arguments(
    arguments: &[OsString],
    maximum_count: usize,
) -> Result<(), clap::Error> {
    if arguments.len() > maximum_count || arguments.iter().any(|a| a.len() > 4096) {
        return Err(invalid());
    }
    Ok(())
}

pub(crate) fn from_matches(matches: &ArgMatches) -> Result<MapsCommand, clap::Error> {
    let (name, args) = matches.subcommand().ok_or_else(invalid)?;
    let location = || coordinate(value(args, "coordinates")?);
    let units = || -> Result<Units, clap::Error> {
        Ok(match value(args, "units")? {
            "metric" => Units::Metric,
            "imperial" => Units::Imperial,
            _ => return Err(invalid()),
        })
    };
    let route_options = || -> Result<RouteOptions, clap::Error> {
        Ok(RouteOptions {
            mode: value(args, "mode")?.into(),
            traffic: value(args, "traffic")?.into(),
            depart_at: args.get_one::<String>("depart-at").cloned(),
        })
    };
    let operation = match name {
        "geocode" => Operation::Geocode {
            address: if let Some(address) = args.get_one::<String>("address") {
                Address::Query(address.clone())
            } else {
                Address::Structured {
                    address_line: value(args, "address-line")?.into(),
                    locality: value(args, "locality")?.into(),
                    admin_district: value(args, "admin-district")?.into(),
                    postal_code: value(args, "postal-code")?.into(),
                    country_region: value(args, "country-region")?.into(),
                }
            },
            top: value(args, "top")?.parse().map_err(|_| invalid())?,
        },
        "reverse" => Operation::Reverse { point: location()? },
        "current" => Operation::CurrentWeather {
            point: location()?,
            units: units()?,
            details: args.get_flag("details"),
        },
        "dailyforecast" => Operation::DailyForecast {
            point: location()?,
            units: units()?,
            duration: value(args, "duration")?.parse().map_err(|_| invalid())?,
        },
        "dailyhistoricalweather" => Operation::DailyHistoricalWeather {
            point: location()?,
            units: units()?,
            start_date: value(args, "start-date")?.into(),
            end_date: value(args, "end-date")?.into(),
        },
        "directions" => Operation::Directions {
            points: points(args, "point")?,
            options: route_options()?,
        },
        "matrix" => Operation::Matrix {
            origins: points(args, "origin")?,
            destinations: points(args, "destination")?,
            options: route_options()?,
        },
        _ => return Err(invalid()),
    };
    let request = operation.prepare().map_err(|_| invalid())?;
    let seconds: u64 = value(matches, "timeout")?.parse().map_err(|_| invalid())?;
    if !(1..=120).contains(&seconds) {
        return Err(invalid());
    }
    let query = matches.get_one::<String>("query").cloned();
    if let Some(query) = &query
        && super::projection::validate(query).is_err()
    {
        return Err(invalid());
    }
    let export = matches
        .get_one::<String>("format")
        .map(|format| -> Result<_, clap::Error> {
            Ok(crate::tabular_export::ExportRequest {
                format: fabric_cli_output::tabular::TabularFormat::parse(format)
                    .map_err(|_| invalid())?,
                output: PathBuf::from(value(matches, "output")?),
            })
        })
        .transpose()?;
    if query.is_some() && export.is_some() {
        return Err(maps_command().error(
            ErrorKind::ArgumentConflict,
            "--query projects terminal data only; exports retain complete result rows.",
        ));
    }
    Ok(MapsCommand {
        request,
        timeout: Duration::from_secs(seconds),
        query,
        export,
        offline_fixture: matches
            .get_one::<String>("offline-fixture")
            .map(PathBuf::from),
        output_format: value(matches, "output-format")?.into(),
    })
}
