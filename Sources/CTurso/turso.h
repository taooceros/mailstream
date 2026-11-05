// Turso additions to the SQLite C API; scripts/turso-multiprocess.patch adds the function.
#pragma once

/// Databases opened after this call coordinate their WAL across processes (experimental).
void turso_enable_multiprocess_wal(void);
