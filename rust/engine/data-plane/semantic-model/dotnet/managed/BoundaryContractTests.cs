// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

// This executable compiles the production entrypoint with a deterministic
// evaluator test double. It never includes or replaces managed/Bpa in production.
using System.Runtime.InteropServices;
using System.Text;
using System.Text.Json;
using Fabric.SemanticModel.Interop;
using Tom = Microsoft.AnalysisServices.Tabular;

namespace Fabric.SemanticModel.Interop.Tests
{
    internal static class BoundaryContractTests
    {
        [StructLayout(LayoutKind.Sequential)]
        private struct NativeCall
        {
            public uint AbiVersion;
            public IntPtr Request;
            public int RequestLength;
            public IntPtr Response;
            public int ResponseCapacity;
            public int ResponseLength;
        }

        private static void Main()
        {
            Equal(1, NativeEntryPoint.Invoke(IntPtr.Zero, 0), "null ABI pointer");
            Equal(1, Invoke("not JSON").Status, "invalid request JSON");
            Equal(1, Invoke("{}").Status, "missing request fields");
            Equal(1, Invoke(Request(options: "[]")).Status, "non-object options");
            Equal(1, Invoke(Request().Replace("\"format\":", "\"schemaVersion\":\"1\",\"format\":")).Status,
                "duplicate request fields");
            Equal(2, Invoke(Request(version: "2")).Status, "unsupported schema");
            Equal(3, Invoke(Request(format: "tmdl")).Status, "unsupported TMDL");
            Equal(4, Invoke(Request(model: "not a BIM database")).Status, "TOM parse failure");
            Equal(4, Invoke(Request(model: "[]")).Status, "non-object model");
            Equal(5, Invoke(Request(options: "{\"boundaryTestMode\":\"throw\"}")).Status, "contained exception");
            Equal(6, Invoke(Request(options: "{\"boundaryTestMode\":\"large\"}")).Status, "large response");
            Equal(6, Invoke(Request(), capacity: 1).Status, "caller response capacity");
            Equal(7, Invoke(Request(options: "{\"boundaryTestMode\":\"bad-json\"}")).Status, "invalid response");
            Equal(1, Invoke(Request(), abiVersion: 2).Status, "incompatible ABI");
            Equal(1, Invoke(Request(), oversizedRequest: true).Status, "request byte bound");
            for (var index = 0; index < 3; index++)
            {
                var (status, response) = Invoke(Request());
                Equal(0, status, "repeated success");
                using var result = JsonDocument.Parse(response);
                if (result.RootElement.GetProperty("schemaVersion").GetString() != "1")
                {
                    throw new InvalidOperationException("Invalid success protocol.");
                }
            }

            Console.WriteLine("PASS: native ABI bounds, TOM loading, response ownership, exception conversion and repeated calls");
        }

        private static string Request(
            string version = "1",
            string format = "bim",
            string model = "{\"name\":\"offline\",\"compatibilityLevel\":1600,\"model\":{\"culture\":\"en-US\"}}",
            string options = "{}")
        {
            return "{\"schemaVersion\":" + JsonSerializer.Serialize(version) +
                ",\"format\":" + JsonSerializer.Serialize(format) +
                ",\"model\":" + JsonSerializer.Serialize(model) +
                ",\"options\":" + options + ",\"evidence\":{}}";
        }

        private static (int Status, string Response) Invoke(
            string request,
            int capacity = 4096,
            uint abiVersion = 1,
            bool oversizedRequest = false)
        {
            var bytes = Encoding.UTF8.GetBytes(request);
            var requestBuffer = Marshal.AllocHGlobal(bytes.Length);
            var responseBuffer = Marshal.AllocHGlobal(capacity);
            var arguments = Marshal.AllocHGlobal(Marshal.SizeOf<NativeCall>());
            try
            {
                Marshal.Copy(bytes, 0, requestBuffer, bytes.Length);
                var call = new NativeCall
                {
                    AbiVersion = abiVersion,
                    Request = requestBuffer,
                    RequestLength = oversizedRequest ? 16 * 1024 * 1024 + 1 : bytes.Length,
                    Response = responseBuffer,
                    ResponseCapacity = capacity,
                };
                Marshal.StructureToPtr(call, arguments, false);
                var status = NativeEntryPoint.Invoke(arguments, Marshal.SizeOf<NativeCall>());
                call = Marshal.PtrToStructure<NativeCall>(arguments);
                if (status != 0)
                {
                    Equal(0, call.ResponseLength, "no successful response after failure");
                    return (status, "");
                }

                if (call.ResponseLength <= 0 || call.ResponseLength > capacity)
                {
                    throw new InvalidOperationException("Invalid response length.");
                }

                var response = new byte[call.ResponseLength];
                Marshal.Copy(responseBuffer, response, 0, response.Length);
                return (status, Encoding.UTF8.GetString(response));
            }
            finally
            {
                Marshal.FreeHGlobal(arguments);
                Marshal.FreeHGlobal(responseBuffer);
                Marshal.FreeHGlobal(requestBuffer);
            }
        }

        private static void Equal(int expected, int actual, string label)
        {
            if (expected != actual)
            {
                throw new InvalidOperationException($"Boundary assertion failed: {label}: {expected} != {actual}.");
            }
        }
    }
}

namespace Fabric.SemanticModel.Bpa
{
    internal static class BpaEngine
    {
        internal static string Analyze(Tom.Database database, JsonElement options, JsonElement evidence)
        {
            if (database.Model is null || evidence.ValueKind != JsonValueKind.Object)
            {
                throw new InvalidOperationException("Invalid TOM test input.");
            }

            var mode = options.TryGetProperty("boundaryTestMode", out var selected) ? selected.GetString() : "";
            return mode switch
            {
                "throw" => throw new InvalidOperationException("sensitive exception text must never cross the ABI"),
                "large" => new string('x', 16 * 1024 * 1024 + 1),
                "bad-json" => "{",
                _ => "{\"schemaVersion\":\"1\",\"source\":{},\"status\":\"test-only\",\"findings\":[],\"rules\":[],\"coverage\":{}}",
            };
        }
    }
}
