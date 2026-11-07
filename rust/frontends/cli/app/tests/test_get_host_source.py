# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Additive end-to-end GET handler/storage observations from immutable source."""

from __future__ import annotations

import base64
import importlib.util
import io
import json
from pathlib import Path
import sys
from types import SimpleNamespace
import unittest
from urllib.parse import urlparse

SPEC = importlib.util.spec_from_file_location(
    "get_options_source", Path(__file__).with_name("test_get_options_source.py")
)
assert SPEC is not None and SPEC.loader is not None
SOURCE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(SOURCE)
load = SOURCE.load


class SourceError(Exception):
    """Offline stand-in for the source's typed Fabric error."""

    def __init__(self, status_code: str) -> None:
        self.status_code = status_code


class GetHostSource(unittest.TestCase):
    """Original handlers execute; no credentials, imports or filesystem authority."""

    def test_metadata_branch_still_enriches_connections_and_schedules(self) -> None:
        events: list[object] = []
        args = SimpleNamespace(query="id")
        handler = load(
            "commands/fs/get/fab_fs_get_item.py", {"exec"},
            {
                "json": json,
                "item_utils": SimpleNamespace(
                    get_item_with_definition=lambda item, args, decode, obtain: (
                        events.append(("acquire", decode, obtain)) or {"id": "i"}
                    )
                ),
                "item_api": SimpleNamespace(
                    get_item_connections=lambda args: (
                        events.append("connections")
                        or SimpleNamespace(text='{"value":[{"id":"c"}]}')
                    )
                ),
                "jobs_api": SimpleNamespace(
                    list_item_schedules=lambda args: (
                        events.append(("schedules", args.jobType))
                        or SimpleNamespace(status_code=200, text='{"value":[{"id":"s"}]}')
                    )
                ),
                "ItemType": SimpleNamespace(ENVIRONMENT="Environment", MIRRORED_DATABASE="MirroredDatabase"),
                "utils_get": SimpleNamespace(
                    is_metadata_property_query=lambda query: query == "id",
                    query_and_export=lambda value, args, name, verbose: events.append(("project", value.copy())),
                ),
            },
        )
        item = SimpleNamespace(
            id="i", workspace=SimpleNamespace(id="w"), item_type="Notebook",
            job_type=SimpleNamespace(value="RunNotebook"), full_name="Book.Notebook"
        )
        actual = handler.exec(item, args)
        self.assertEqual(events[:3], [
            ("acquire", True, False), "connections", ("schedules", "RunNotebook")
        ])
        self.assertEqual(actual, {"id": "i", "connections": [{"id": "c"}], "schedules": [{"id": "s"}]})

    def test_environment_enrichment_exceptions_use_lists_but_non200_is_omitted(self) -> None:
        events: list[str] = []
        replies = iter([
            SimpleNamespace(status_code=200, text='{"custom":"published"}'),
            RuntimeError("offline service failure"),
            SimpleNamespace(status_code=204, text="ignored"),
            SimpleNamespace(status_code=200, text="malformed"),
        ])

        def acquire(args: SimpleNamespace, **kwargs: object) -> SimpleNamespace:
            events.append(args.ext_uri)
            reply = next(replies)
            if isinstance(reply, Exception):
                raise reply
            return reply

        helper = load(
            "utils/fab_cmd_get_utils.py", {"get_environment_metadata"},
            {"json": json, "item_api": SimpleNamespace(get_item=acquire)}
        )
        actual = helper.get_environment_metadata({"id": "i"}, SimpleNamespace())
        self.assertEqual(events, [
            "/libraries", "/staging/libraries", "/sparkcompute", "/staging/sparkcompute"
        ])
        self.assertEqual(actual, {
            "id": "i", "published": {"libraries": {"custom": "published"}},
            "staging": {"libraries": [], "sparkComputeSettings": []}
        })

    def test_mirroring_status_and_tables_have_distinct_shapes_and_fallbacks(self) -> None:
        helper = load(
            "utils/fab_cmd_get_utils.py", {"get_mirroreddb_metadata"},
            {
                "json": json,
                "mirroring_api": SimpleNamespace(
                    get_mirroring_status=lambda args: SimpleNamespace(status_code=200, text='{"state":"Running"}'),
                    get_table_mirroring_status=lambda args: SimpleNamespace(status_code=200, text='{"value":[]}'),
                ),
            },
        )
        self.assertEqual(helper.get_mirroreddb_metadata({}, SimpleNamespace()), {
            "status": {"state": "Running"}, "tablesStatus": []
        })

    def test_source_forces_internal_export_and_only_unsupported_command_falls_back(self) -> None:
        for failure, expected in [(None, "definition"), ("UnsupportedCommand", "metadata")]:
            events: list[object] = []

            def support(command: str) -> bool:
                events.append(("support", command))
                if failure:
                    raise SourceError(failure)
                return True

            helper = load(
                "utils/fab_item_util.py", {"get_item_with_definition"},
                {
                    "json": json,
                    "format_mapping": {"Notebook": "notebooks"},
                    "Command": SimpleNamespace(FS_EXPORT="export"),
                    "FabricCLIError": SourceError,
                    "fab_constant": SimpleNamespace(ERROR_UNSUPPORTED_COMMAND="UnsupportedCommand"),
                    "export_item": SimpleNamespace(
                        export_single_item=lambda item, args, **kw: (
                            events.append((args.force, kw)) or {"branch": "definition"}
                        )
                    ),
                    "item_api": SimpleNamespace(
                        get_item=lambda args, **kw: SimpleNamespace(text='{"branch":"metadata"}')
                    ),
                },
            )
            args = SimpleNamespace(force=False)
            item = SimpleNamespace(
                workspace=SimpleNamespace(id="w"), id="i", item_type="Notebook",
                check_command_support=support,
            )
            self.assertEqual(helper.get_item_with_definition(item, args), {"branch": expected})
            self.assertTrue(args.force)
            self.assertEqual(args.item_uri, "notebooks")
            self.assertEqual(events[0], ("support", "export"))
            if failure is None:
                self.assertEqual(events[1], (True, {"do_export": False, "decode": True, "item_uri": True}))

    def storage(self) -> tuple[SimpleNamespace, list[tuple[str, str]], list[str]]:
        writes: list[tuple[str, str]] = []
        messages: list[str] = []

        class Capture(io.StringIO):
            def __init__(self, path: str) -> None:
                super().__init__()
                self.path = path

            def close(self) -> None:
                if not self.closed:
                    writes.append((self.path, self.getvalue()))
                super().close()

        namespace = {
            "json": json,
            "datetime": SimpleNamespace(now=lambda: SimpleNamespace(strftime=lambda fmt: "20000102_030405")),
            "os": SimpleNamespace(
                path=SimpleNamespace(
                    join=lambda *parts: "/".join(parts), normpath=lambda value: value,
                    dirname=lambda value: "destination", exists=lambda value: True,
                )
            ),
            "open": lambda path, *args, **kw: Capture(path),
            "fab_ui": SimpleNamespace(
                print_output_format=lambda *args, **kwargs: None,
                print_grey=lambda text, *args: messages.append(text),
            ),
            "get_export_path": lambda output: {"type": "local", "path": output},
        }
        storage = load(
            "utils/fab_storage.py",
            {"do_output", "write_to_storage", "_create_get_export_file_name", "_write_to_local", "_validate_json"},
            namespace,
        )
        return storage, writes, messages

    def test_actual_do_output_passes_unsuffixed_path_to_writer_one_suffix_on_disk(self) -> None:
        storage, writes, messages = self.storage()
        storage.do_output({"name": "café"}, "Book.Notebook", SimpleNamespace(output="destination"))
        self.assertEqual(writes, [(
            "destination/Book.Notebook/get_20000102_030405.json",
            '{\n    "name": "café"\n}',
        )])
        self.assertEqual(messages, [
            "Exporting result to 'destination/Book.Notebook/get_20000102_030405.json'..."
        ])

    def test_empty_projected_values_are_written_without_an_envelope(self) -> None:
        storage, writes, _ = self.storage()
        for data, text, suffix in [
            ([], "[]", ".json"), ({}, "{}", ".json"), ("", "", ""),
            (False, "False", ""), (0, "0", ""), (None, "None", ""),
            ("false", "false", ".json"),
        ]:
            storage.do_output(data, "Book.Notebook", SimpleNamespace(output="destination"))
            self.assertEqual(writes.pop(), (
                f"destination/Book.Notebook/get_20000102_030405{suffix}", text
            ))

    def test_query_scalar_stringification_precedes_storage_json_classification(self) -> None:
        storage, writes, _ = self.storage()
        data = {"zero": 0, "false": False, "null": None, "empty": "", "list": []}
        query = load(
            "utils/fab_jmespath.py", {"search"},
            {"jmespath": SimpleNamespace(
                search=lambda expression, value, options: value[expression]
            )},
        )
        handler = load(
            "utils/fab_cmd_get_utils.py", {"query_and_export"},
            {"utils_jmespath": query, "utils_storage": storage},
        )
        for expression, expected, suffix in [
            ("zero", "0", ".json"), ("false", "False", ""),
            ("null", "None", ""), ("empty", "", ""), ("list", "[]", ".json"),
        ]:
            handler.query_and_export(
                data, SimpleNamespace(query=expression, output="destination"), "Book.Notebook"
            )
            self.assertEqual(writes.pop(), (
                f"destination/Book.Notebook/get_20000102_030405{suffix}", expected
            ))

    def test_decoded_payload_type_is_replaced_not_removed(self) -> None:
        decoder = load(
            "utils/fab_cmd_export_utils.py", {"decode_payload"},
            {"base64": base64, "json": json},
        )
        value = {"definition": {"parts": [{
            "path": "value.json", "payload": "eyJhIjoxfQ==",
            "payloadType": "InlineBase64", "error": "retained",
        }]}}
        self.assertEqual(decoder.decode_payload(value), {
            "definition": {"parts": [{
                "path": "value.json", "payload": {"a": 1},
                "payloadType": "DecodeBase64", "error": "retained",
            }]}
        })

    def test_terminal_poll_location_controls_result_fetch_not_initial_location(self) -> None:
        for initial_location in [False, True]:
            for terminal_location in [False, True]:
                events: list[str] = []
                original = SimpleNamespace(
                    status_code=202,
                    headers={"Location": "https://api.fabric.microsoft.com/v1/operations/op"}
                    if initial_location else {},
                )
                terminal = SimpleNamespace(
                    status_code=200,
                    headers={"Location": "https://api.fabric.microsoft.com/v1/operations/op/result"}
                    if terminal_location else {},
                    json=lambda: {"status": "Succeeded"},
                )
                result = SimpleNamespace(status_code=200, headers={})

                def request(args: SimpleNamespace, **kwargs: object) -> SimpleNamespace:
                    events.append(args.uri)
                    return result if args.uri.endswith("/result") else terminal

                poller = load(
                    "client/fab_api_client.py", {"_poll_operation", "_fetch_operation_result"},
                    {
                        "Namespace": SimpleNamespace,
                        "do_request": request,
                        "get_polling_interval": lambda headers: 0,
                        "time": SimpleNamespace(sleep=lambda delay: None),
                        "fab_logger": SimpleNamespace(log_progress=lambda status: None),
                        "fab_constant": SimpleNamespace(
                            SCOPE_FABRIC_DEFAULT="fabric", SCOPE_AZURE_DEFAULT="azure",
                            API_ENDPOINT_FABRIC="api.fabric.microsoft.com",
                        ),
                        "urlparse": urlparse,
                    },
                )
                actual = poller._poll_operation(
                    "fabric", "operations/op", original, "fabric", True
                )
                self.assertIs(actual, result if terminal_location else original)
                self.assertEqual(events, ["operations/op"] + (
                    ["operations/op/result"] if terminal_location else []
                ))
                if not terminal_location:
                    self.assertEqual(original.status_code, 200)


if __name__ == "__main__":
    result = unittest.TextTestRunner(verbosity=2).run(
        unittest.defaultTestLoader.loadTestsFromTestCase(GetHostSource)
    )
    print(json.dumps({
        "source_pin": SOURCE.PIN, "sources": SOURCE.DIGESTS,
        "end_to_end_local_json_suffixes": 1,
        "requested_local_json_suffixes": 2,
        "suffix_decision_requires_independent_review": True,
    }, sort_keys=True))
    sys.exit(0 if result.wasSuccessful() else 1)
