# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Actual hidden-resource singleton routes from the exact Python authority."""

import argparse
import json
import sys
from pathlib import Path
from types import SimpleNamespace

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "access"))
from reference import SOURCE, source_module

HERE = Path(__file__).resolve().parent


def observations() -> list[dict]:
    """Execute every assigned hidden metadata route, including helper dispatch."""
    result = []
    for name, file, function in [
        ("connection", "fab_api_connection.py", "get_connection"),
        ("gateway", "fab_api_gateway.py", "get_gateway"),
        ("domain", "fab_api_domain.py", "get_domain"),
        ("spark-pool", "fab_api_sparkpool.py", "get_spark_pool"),
        ("private-endpoint", "fab_api_managedprivateendpoint.py", "get_managed_private_endpoint"),
        ("share", "fab_api_item.py", "get_item_external_data_share"),
    ]:
        requests = []
        fabric_api = SimpleNamespace(do_request=lambda args: requests.append(
            {"method": args.method.upper(), "url": f"https://api.fabric.microsoft.com/v1/{args.uri}"}
        ))
        item_api = SimpleNamespace(**source_module("client/fab_api_item.py", {"fabric_api": fabric_api}))
        module = source_module(f"client/{file}", {"fabric_api": fabric_api, "item_api": item_api})
        module[function](argparse.Namespace(ws_id="workspace", item_id="item", id="resource"))
        result.append({"name": name, "request": requests[0]})
    return result


def main() -> None:
    """Capture once or verify; never overwrite source expectations."""
    document = {"source_commit": SOURCE, "cases": observations()}
    parser = argparse.ArgumentParser()
    parser.add_argument("--initial-capture", action="store_true")
    args = parser.parse_args()
    path = HERE / "metadata_cases.json"
    if args.initial_capture:
        if path.exists():
            raise SystemExit("Refusing to overwrite hidden metadata oracle")
        path.write_text(json.dumps(document, indent=2) + "\n", encoding="utf-8")
    else:
        assert document == json.loads(path.read_text(encoding="utf-8")), "Hidden metadata source drift"
    print(f"hidden singleton source routes: {len(document['cases'])} passed")


if __name__ == "__main__":
    main()
