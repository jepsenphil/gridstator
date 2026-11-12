The snapshot matcher follows picomatch 4.0.7 behavior captured in [workspace-globs.json](../../../contracts/workspace-globs.json). Native tests consume these behavior cases; the executable does not run JavaScript.

The ordinary UTF16 pattern VM handles glob paths. Raw capture/assertion/backreference syntax emits an ECMAScript expression for the Rust-only `regress` backend. Its source is pinned to [95c04041cd9b30249961eb80d950102c29c9586c](https://github.com/ridiculousfish/regress/tree/95c04041cd9b30249961eb80d950102c29c9586c), with only `std` and `utf16` features. This revision provides `find_from_ucs2_budgeted` and distinguishes exhausted matching from a negative result; the published 0.12.0 API does not expose that execution budget. The dependency is licensed MIT OR Apache-2.0.

The backend must use `Regex::from_unicode` with emitted UCS2 units, `unicode:false` and `no_opt:true`; ordinary UTF8 parsing would merge non-BMP literals, and the optimizer can amplify compilation. Parser depth, emitted units, visits and capture count have independent limits. Each path shares a 2,000,000-unit reservation across all patterns. The upstream API counts backtracking events, so the native allowance is divided by `(emitted units + 2*capture slots + 1)*(subject UTF16 units +1)` and charges the complete reservation. This conservatively accounts for forward-program/capture restoration work; it is not an exact interpreter-instruction counter. Exhaustion is an explicit error. Keep the capture-heavy nested-plus regression when changing this accounting.

The picomatch parser informs the glob grammar and repeated-extglob safety rewrites. Its attribution and license are retained below.

The MIT License (MIT)

Copyright (c) 2017-present, Jon Schlinkert.

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in
all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
THE SOFTWARE.
