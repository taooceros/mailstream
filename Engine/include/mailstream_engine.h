// The embedded himalaya engine (Engine/src/engine.rs).
#pragma once

#include <stddef.h>
#include <stdint.h>

/// Receives one request's exit status and output, on the request's thread. `output` is only
/// valid during the call.
typedef void (*ms_engine_callback)(void *_Nullable context, int32_t status,
                                   const uint8_t *_Nullable output, size_t length);

/// Prepares the process (ignores SIGPIPE). Call once before the first request.
void ms_engine_init(void);

/// Runs one request, a JSON array of himalaya arguments, on its own thread; `callback` receives
/// `context` back exactly once.
void ms_engine_run(const uint8_t *_Nonnull request, size_t length, void *_Nullable context,
                   ms_engine_callback _Nonnull callback);
