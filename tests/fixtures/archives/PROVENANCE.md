# Archive regression fixtures

RAR inputs are from the crates.io source package `rars 0.13.0`,
<https://github.com/bitplane/rars>, under Apache-2.0. Its
`tests/fixtures/LICENSES.md` explicitly identifies these inputs as synthetic
project-created fixtures, not third-party content. The license is reproduced in
`RAR-LICENSE.txt`.

| Local name | Upstream path under `tests/fixtures/` | Changes |
|---|---|---|
| rar4-stored.rar | rar15_40/rar420/ext_time_rar420.rar | hello.txt → page2.png |
| rar4-solid.rar | rar15_40/rar300/solid_simple_rar300.rar | one.txt → p10.png; two.txt → p02.png |
| rar5-stored.rar | rar50/stored.rar | hello.txt → page2.png |
| rar5-solid.rar | rar50/solid.rar | hello.txt → page2.png; tiny.txt → page.png |
| rar4-encrypted-payload.rar | rar15_40/encrypted/rar4_password.rar | Unmodified |
| rar4-encrypted-headers.rar | rar15_40/encrypted/rar4_header_encrypted.rar | Unmodified |
| rar5-encrypted-payload.rar | rar50/password_aes.rar | Unmodified |
| rar5-encrypted-headers.rar | rar50/header_encrypted.rar | Unmodified |

Renames preserve filename lengths and compressed payloads. Only filename bytes
and their enclosing header CRCs were changed (RAR4: CRC32 low 16 bits over the
header after its CRC; RAR5: CRC32 over the header-size field and header body).
Payloads are synthetic **text**, not image-decoding fixtures; image extensions
exercise the virtual source's filtering, indexing and bounded byte reads.
RAR4's stored sample was produced with RAR 4.20; the solid legacy sample was
produced with RAR 3.00 and uses the same RAR3/4 reader family.

7z fixtures are generated in the Rust unit tests using `sevenz-rust2 0.25.0`'s
pure-Rust writer. Their payloads are project-created byte strings or a generated
tiny PNG; no external files/programs are needed. The test code and synthetic
payloads use Oxeye's MIT license. No UnRAR/native RAR code is included or run.
