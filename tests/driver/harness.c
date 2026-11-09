// Functional test for the Soundpad Virtual Mic HAL plug-in without installing it.
//
// Loads the compiled driver the same way coreaudiod does (factory function +
// AudioServerPlugInDriverInterface), with a mock host, then:
//   1. checks user-visible names / flags (no "BlackHole" branding leaks),
//   2. writes audio as an app playing to the device (WriteMix) and reads it back
//      as an app recording from it (ReadInput) across many cycles, including
//      the ring-buffer wrap-around, and requires a bit-exact round trip,
//   3. checks that the input is silent when nobody is playing.
//
// Build/run: scripts/test_driver.sh

#include <CoreAudio/AudioServerPlugIn.h>
#include <dlfcn.h>
#include <math.h>
#include <stdio.h>
#include <string.h>

enum { kPlugIn = 1, kBox = 2, kDevice = 3, kStreamIn = 4, kStreamOut = 7, kDevice2 = 12 };

static int failures = 0;
#define CHECK(cond, ...)                                  \
    do {                                                  \
        if (cond) { printf("  ok   "); }                  \
        else { printf("  FAIL "); failures++; }           \
        printf(__VA_ARGS__);                              \
        printf("\n");                                     \
    } while (0)

static OSStatus host_props_changed(AudioServerPlugInHostRef h, AudioObjectID o, UInt32 n, const AudioObjectPropertyAddress *a) { return 0; }
static OSStatus host_copy(AudioServerPlugInHostRef h, CFStringRef k, CFPropertyListRef *out) { *out = NULL; return 0; }
static OSStatus host_write(AudioServerPlugInHostRef h, CFStringRef k, CFPropertyListRef d) { return 0; }
static OSStatus host_delete(AudioServerPlugInHostRef h, CFStringRef k) { return 0; }
static OSStatus host_reqcfg(AudioServerPlugInHostRef h, AudioObjectID o, UInt64 a, void *i) { return 0; }

static AudioServerPlugInHostInterface host = {
    host_props_changed, host_copy, host_write, host_delete, host_reqcfg,
};

static AudioServerPlugInDriverRef drv;

static int get_string(AudioObjectID obj, AudioObjectPropertySelector sel, char *buf, size_t len) {
    AudioObjectPropertyAddress a = {sel, kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyElementMain};
    CFStringRef s = NULL;
    UInt32 size = 0;
    OSStatus e = (*drv)->GetPropertyData(drv, obj, 0, &a, 0, NULL, sizeof(CFStringRef), &size, &s);
    if (e || !s) return 0;
    CFStringGetCString(s, buf, (CFIndex)len, kCFStringEncodingUTF8);
    CFRelease(s);
    return 1;
}

static UInt32 get_u32(AudioObjectID obj, AudioObjectPropertySelector sel, AudioObjectPropertyScope scope) {
    AudioObjectPropertyAddress a = {sel, scope, kAudioObjectPropertyElementMain};
    UInt32 v = 0xFFFFFFFF, size = 0;
    (*drv)->GetPropertyData(drv, obj, 0, &a, 0, NULL, sizeof(v), &size, &v);
    return v;
}

static void io(UInt32 op, Float64 t, UInt32 frames, float *buf) {
    AudioServerPlugInIOCycleInfo c;
    memset(&c, 0, sizeof c);
    c.mNominalIOBufferFrameSize = frames;
    c.mCurrentTime.mSampleTime = t;
    c.mInputTime.mSampleTime = t;
    c.mOutputTime.mSampleTime = t;
    AudioObjectID stream = op == kAudioServerPlugInIOOperationReadInput ? kStreamIn : kStreamOut;
    (*drv)->BeginIOOperation(drv, kDevice, 1, op, frames, &c);
    (*drv)->DoIOOperation(drv, kDevice, stream, 1, op, frames, &c, buf, NULL);
    (*drv)->EndIOOperation(drv, kDevice, 1, op, frames, &c);
}

int main(int argc, char **argv) {
    if (argc < 2) {
        fprintf(stderr, "usage: harness <path to driver executable>\n");
        return 2;
    }
    void *lib = dlopen(argv[1], RTLD_NOW | RTLD_LOCAL);
    if (!lib) {
        printf("FAIL dlopen: %s\n", dlerror());
        return 1;
    }
    void *(*create)(CFAllocatorRef, CFUUIDRef) = dlsym(lib, "BlackHole_Create");
    CHECK(create != NULL, "factory function exported");
    if (!create) return 1;

    drv = create(NULL, kAudioServerPlugInTypeUUID);
    CHECK(drv != NULL, "factory returns a driver for the AudioServerPlugIn type");
    if (!drv) return 1;
    CHECK((*drv)->Initialize(drv, &host) == 0, "Initialize with mock host");

    printf("\n[identity]\n");
    char s[256];
    get_string(kDevice, kAudioObjectPropertyName, s, sizeof s);
    CHECK(strcmp(s, "Soundpad Virtual Mic") == 0, "device name: \"%s\"", s);
    get_string(kDevice, kAudioDevicePropertyDeviceUID, s, sizeof s);
    CHECK(strstr(s, "Soundpad") != NULL, "device UID: \"%s\"", s);
    get_string(kDevice, kAudioObjectPropertyManufacturer, s, sizeof s);
    CHECK(strstr(s, "Soundpad") != NULL, "manufacturer: \"%s\"", s);
    get_string(kBox, kAudioObjectPropertyName, s, sizeof s);
    CHECK(strstr(s, "BlackHole") == NULL, "box name: \"%s\"", s);
    get_string(kBox, kAudioObjectPropertyModelName, s, sizeof s);
    CHECK(strstr(s, "BlackHole") == NULL, "box model: \"%s\"", s);
    CHECK(get_u32(kDevice, kAudioDevicePropertyDeviceCanBeDefaultSystemDevice, kAudioObjectPropertyScopeOutput) == 0,
          "never becomes the system alert-sound device");
    CHECK(get_u32(kDevice, kAudioDevicePropertyIsHidden, kAudioObjectPropertyScopeGlobal) == 0, "main device visible");
    CHECK(get_u32(kDevice2, kAudioDevicePropertyIsHidden, kAudioObjectPropertyScopeGlobal) == 1, "mirror device hidden");

    AudioObjectPropertyAddress ra = {kAudioDevicePropertyNominalSampleRate, kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyElementMain};
    Float64 rate = 0;
    UInt32 size = 0;
    (*drv)->GetPropertyData(drv, kDevice, 0, &ra, 0, NULL, sizeof rate, &size, &rate);
    CHECK(rate == 44100 || rate == 48000, "nominal sample rate %.0f Hz", rate);

    AudioObjectPropertyAddress fa = {kAudioStreamPropertyVirtualFormat, kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyElementMain};
    AudioStreamBasicDescription f;
    (*drv)->GetPropertyData(drv, kStreamIn, 0, &fa, 0, NULL, sizeof f, &size, &f);
    CHECK(f.mChannelsPerFrame == 2 && (f.mFormatFlags & kAudioFormatFlagIsFloat), "stream format: %u ch float32",
          (unsigned)f.mChannelsPerFrame);

    printf("\n[audio path: app output -> driver -> app input]\n");
    CHECK((*drv)->StartIO(drv, kDevice, 1) == 0, "StartIO");

    enum { N = 512 };
    float out[N * 2], in[N * 2];
    // Before anyone plays, recording must be silent.
    io(kAudioServerPlugInIOOperationReadInput, 0, N, in);
    int silent = 1;
    for (int i = 0; i < N * 2; i++) silent &= in[i] == 0.0f;
    CHECK(silent, "input is silent when nothing plays");

    // Stream ~3 s (crosses the 65536-frame ring buffer wrap twice).
    long mismatches = 0, cycles = 0;
    double phase = 0;
    for (Float64 t = N; t < 48000 * 3; t += N, cycles++) {
        for (int i = 0; i < N; i++) {
            float v = (float)(0.5 * sin(phase));
            phase += 2 * M_PI * 1000.0 / 48000.0;
            out[i * 2] = v;
            out[i * 2 + 1] = -v; // distinct channels to catch L/R swaps
        }
        io(kAudioServerPlugInIOOperationWriteMix, t, N, out);
        io(kAudioServerPlugInIOOperationReadInput, t, N, in);
        for (int i = 0; i < N * 2; i++) mismatches += in[i] != out[i];
    }
    CHECK(mismatches == 0, "%ld cycles of 1 kHz stereo audio round-trip bit-exact (%ld mismatched samples)", cycles,
          mismatches);

    CHECK((*drv)->StopIO(drv, kDevice, 1) == 0, "StopIO");

    printf("\n%s (%d failure%s)\n", failures ? "DRIVER TEST FAILED" : "DRIVER TEST PASSED", failures,
           failures == 1 ? "" : "s");
    return failures ? 1 : 0;
}
