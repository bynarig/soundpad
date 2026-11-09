// C interface to the Rust core (src/ffi.rs).
#pragma once
#include <stdbool.h>
#include <stdint.h>

typedef struct SPCore SPCore;
typedef void (*SPWakeFn)(void *ctx);

typedef struct {
    uint64_t sound_id; // 0 = nothing playing
    double position;   // seconds
    double duration;   // seconds
    bool paused;
    uint32_t voices;
    float peak;        // 0..1 output level
} SPPlayback;

// Create the core on the main thread. config_path may be NULL.
// `wake` may be called from any thread when the UI should refresh.
SPCore *sp_core_new(const char *config_path, SPWakeFn wake, void *ctx);
void sp_core_free(SPCore *core);
// Housekeeping; returns true when the state changed.
bool sp_core_tick(SPCore *core);
// JSON snapshot; free with sp_string_free.
char *sp_core_state(SPCore *core);
// JSON command; returns NULL on success or an error message (free it).
char *sp_core_command(SPCore *core, const char *json);
void sp_core_playback(SPCore *core, SPPlayback *out);
void sp_string_free(char *s);
// CLI modes; returns exit code or -1 to start the GUI.
int32_t sp_cli_main(void);
