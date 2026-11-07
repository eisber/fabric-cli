# Copyright (c) Microsoft Corporation. Licensed under the MIT License.
"""Capture Python Unicode simple-fold/uppercase differences for the offline C# port."""

import unicodedata
from pathlib import Path


def main() -> None:
    """Generate static data only; the product never invokes Python."""
    folding = []
    expanding = []
    for scalar in range(0x110000):
        char = chr(scalar)
        fold = char.casefold()
        if len(fold) == 1 and fold != char.lower():
            folding.append((scalar, ord(fold)))
        if len(char.upper()) > 1:
            expanding.append(scalar)
    lines = [
        "// Copyright (c) Microsoft Corporation. Licensed under the MIT License.",
        "// Generated development-time Python Unicode " + unicodedata.unidata_version + " data.",
        "using System.Collections.Generic;",
        "",
        "namespace Fabric.SemanticModel.Bpa;",
        "",
        "internal static class UnicodeCaseData",
        "{",
        "    internal static readonly Dictionary<int, int> SimpleFold = new()",
        "    {",
    ]
    lines.extend(f"        [0x{source:X}] = 0x{target:X}," for source, target in folding)
    # Python re.I treats these as equivalent to ASCII i, without expanding dotted I.
    lines.extend(["        [0x130] = 0x69,", "        [0x131] = 0x69,", "    };"])
    lines.extend(["    internal static readonly HashSet<int> ExpandingUpper = new()", "    {"])
    lines.extend("        " + ", ".join(f"0x{v:X}" for v in expanding[i:i + 8]) + ","
                 for i in range(0, len(expanding), 8))
    lines.extend(["    };", "}", ""])
    (Path(__file__).parent.parent / "UnicodeCaseData.g.cs").write_text("\n".join(lines), encoding="utf-8")
    print(f"Captured Unicode {unicodedata.unidata_version}: {len(folding)} simple-fold exceptions; {len(expanding)} expanding uppercase mappings.")


if __name__ == "__main__":
    main()
