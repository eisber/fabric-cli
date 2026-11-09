// Copyright (c) Microsoft Corporation. Licensed under the MIT License.
using System;
using System.Text;
using System.Text.RegularExpressions;

namespace Fabric.SemanticModel.Bpa;

internal static class PythonRegex
{
    internal static string Fold(string value, bool pattern = false)
    {
        var result = new StringBuilder(value.Length);
        bool escaped = false;
        foreach (var rune in value.EnumerateRunes())
        {
            if (pattern && !escaped && rune.Value == '\\')
            {
                result.Append('\\');
                escaped = true;
                continue;
            }
            if (escaped)
            {
                result.Append(rune.ToString());
                escaped = false;
                continue;
            }
            result.Append(UnicodeCaseData.SimpleFold.TryGetValue(rune.Value, out int replacement)
                ? new Rune(replacement).ToString() : Rune.ToLowerInvariant(rune).ToString());
        }
        return result.ToString();
    }

    internal static string Whitespace(string pattern)
    {
        var result = new StringBuilder(pattern.Length);
        for (int i = 0; i < pattern.Length; i++)
        {
            if (pattern[i] == '\\' && i + 1 < pattern.Length)
            {
                char escaped = pattern[++i];
                if (escaped == 's') result.Append(@"[\s\u001c-\u001f]");
                else result.Append('\\').Append(escaped);
            }
            else result.Append(pattern[i]);
        }
        return result.ToString();
    }

    internal static string StripWhitespace(string value, TimeSpan timeout) =>
        Regex.Replace(value, @"[\s\u001c-\u001f]+", "", RegexOptions.CultureInvariant, timeout);

    internal static string Trim(string value)
    {
        int start = 0, end = value.Length;
        while (start < end && IsWhitespace(value[start])) start++;
        while (end > start && IsWhitespace(value[end - 1])) end--;
        return value[start..end];
    }

    private static bool IsWhitespace(char value) => char.IsWhiteSpace(value) || value is >= '\u001c' and <= '\u001f';
}
