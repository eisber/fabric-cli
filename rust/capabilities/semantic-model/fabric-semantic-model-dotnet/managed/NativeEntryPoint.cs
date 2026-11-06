// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

using System.Runtime.InteropServices;
using System.Text;
using System.Text.Json;
using Fabric.SemanticModel.Bpa;
using Tom = Microsoft.AnalysisServices.Tabular;

namespace Fabric.SemanticModel.Interop;

/// <summary>Private-to-the-host ABI, using exclusively caller-owned buffers.</summary>
public static class NativeEntryPoint
{
    private const int MaxRequestBytes = 16 * 1024 * 1024;
    private const int MaxResponseBytes = 16 * 1024 * 1024;
    private static readonly UTF8Encoding StrictUtf8 = new(false, true);

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

    // Values are shared with the Rust private interop module.
    private enum Status
    {
        Success = 0,
        InvalidRequest = 1,
        UnsupportedSchema = 2,
        UnsupportedFormat = 3,
        MalformedModel = 4,
        ManagedFailure = 5,
        ResponseTooLarge = 6,
        InvalidResponse = 7,
    }

    /// <summary>
    /// Hostfxr ComponentEntryPoint. Never retains pointers or propagates managed
    /// exceptions; all memory remains owned and released by Rust.
    /// </summary>
    public static int Invoke(IntPtr arguments, int sizeBytes)
    {
        try
        {
            if (arguments == IntPtr.Zero || sizeBytes != Marshal.SizeOf<NativeCall>())
            {
                return (int)Status.InvalidRequest;
            }

            var call = Marshal.PtrToStructure<NativeCall>(arguments);
            if (call.AbiVersion != 1 || call.Request == IntPtr.Zero ||
                call.Response == IntPtr.Zero || call.RequestLength <= 0 ||
                call.RequestLength > MaxRequestBytes ||
                call.ResponseCapacity <= 0 || call.ResponseCapacity > MaxResponseBytes)
            {
                return (int)Status.InvalidRequest;
            }

            call.ResponseLength = 0;
            Marshal.StructureToPtr(call, arguments, false);
            var requestBytes = new byte[call.RequestLength];
            Marshal.Copy(call.Request, requestBytes, 0, requestBytes.Length);
            var status = Analyze(requestBytes, out var response);
            if (status != Status.Success)
            {
                return (int)status;
            }

            var count = StrictUtf8.GetByteCount(response!);
            if (count > call.ResponseCapacity)
            {
                return (int)Status.ResponseTooLarge;
            }

            var bytes = StrictUtf8.GetBytes(response!);
            Marshal.Copy(bytes, 0, call.Response, bytes.Length);
            call.ResponseLength = bytes.Length;
            Marshal.StructureToPtr(call, arguments, false);
            return (int)Status.Success;
        }
        catch (Exception)
        {
            // Deliberately never expose exception text, model data or evidence.
            return (int)Status.ManagedFailure;
        }
    }
    private static Status Analyze(byte[] requestBytes, out string? response)
    {
        response = null;
        JsonDocument request;
        try
        {
            request = JsonDocument.Parse(
                StrictUtf8.GetString(requestBytes),
                new JsonDocumentOptions { MaxDepth = 128 });
        }
        catch (Exception exception) when (exception is JsonException or DecoderFallbackException)
        {
            return Status.InvalidRequest;
        }

        using (request)
        {
            var root = request.RootElement;
            if (root.ValueKind != JsonValueKind.Object)
            {
                return Status.InvalidRequest;
            }

            var names = new HashSet<string>(StringComparer.Ordinal);
            foreach (var property in root.EnumerateObject())
            {
                if (!names.Add(property.Name) ||
                    property.Name is not ("schemaVersion" or "format" or "model" or "options" or "evidence"))
                {
                    return Status.InvalidRequest;
                }
            }

            if (names.Count != 5 ||
                !root.TryGetProperty("schemaVersion", out var version) ||
                version.ValueKind != JsonValueKind.String ||
                !root.TryGetProperty("format", out var format) ||
                format.ValueKind != JsonValueKind.String ||
                !root.TryGetProperty("model", out var model) ||
                model.ValueKind != JsonValueKind.String ||
                !root.TryGetProperty("options", out var options) ||
                options.ValueKind != JsonValueKind.Object ||
                !root.TryGetProperty("evidence", out var evidence) ||
                evidence.ValueKind != JsonValueKind.Object)
            {
                return Status.InvalidRequest;
            }

            if (version.GetString() != "1")
            {
                return Status.UnsupportedSchema;
            }

            if (format.GetString() != "bim")
            {
                return Status.UnsupportedFormat;
            }

            var serialized = model.GetString();
            if (string.IsNullOrEmpty(serialized))
            {
                return Status.InvalidRequest;
            }

            Tom.Database database;
            try
            {
                // This is a standalone BIM database, not a TMSL command envelope.
                // Parse once before TOM to bound nesting and require an object.
                using var modelJson = JsonDocument.Parse(
                    serialized, new JsonDocumentOptions { MaxDepth = 128 });
                if (modelJson.RootElement.ValueKind != JsonValueKind.Object)
                {
                    return Status.MalformedModel;
                }

                database = Tom.JsonSerializer.DeserializeDatabase(serialized);
                if (database is null || database.Model is null)
                {
                    return Status.MalformedModel;
                }
            }
            catch (Exception)
            {
                return Status.MalformedModel;
            }

            // No Server.Connect, credentials, DMV queries, or model mutation.
            response = BpaEngine.Analyze(database, options, evidence);
            if (response is null)
            {
                return Status.InvalidResponse;
            }

            if (response.Length > MaxResponseBytes || StrictUtf8.GetByteCount(response) > MaxResponseBytes)
            {
                return Status.ResponseTooLarge;
            }

            try
            {
                using var output = JsonDocument.Parse(
                    response, new JsonDocumentOptions { MaxDepth = 128 });
                var result = output.RootElement;
                if (result.ValueKind != JsonValueKind.Object ||
                    !result.TryGetProperty("schemaVersion", out var schema) ||
                    schema.ValueKind != JsonValueKind.String || schema.GetString() != "1" ||
                    !result.TryGetProperty("source", out var source) || source.ValueKind != JsonValueKind.Object ||
                    !result.TryGetProperty("status", out var state) || state.ValueKind != JsonValueKind.String ||
                    !result.TryGetProperty("findings", out var findings) || findings.ValueKind != JsonValueKind.Array ||
                    !result.TryGetProperty("rules", out var rules) || rules.ValueKind != JsonValueKind.Array ||
                    !result.TryGetProperty("coverage", out var coverage) || coverage.ValueKind != JsonValueKind.Object)
                {
                    return Status.InvalidResponse;
                }
            }
            catch (JsonException)
            {
                return Status.InvalidResponse;
            }

            return Status.Success;
        }
    }
}
