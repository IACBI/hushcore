/*
 * End-to-end check of the HushCore virtual devices.
 *
 *   loopback_test --list             print both HushCore devices and exit
 *   loopback_test                    play a 440 Hz tone at -12 dBFS into HushCore Sink,
 *                                    record HushCore Microphone for 2.5 s, verify the tone
 *   loopback_test --delivery-only    as above, but only require that both IO procs ran and
 *                                    the microphone delivered buffers (no signal check).
 *                                    Use when the runner's microphone permission can
 *                                    silence the recording.
 *
 * Exit status is 0 on success and 1 on any failure.
 */

#include <CoreAudio/CoreAudio.h>
#include <CoreFoundation/CoreFoundation.h>
#include <math.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#define SINK_UID "HushCoreSink_UID"
#define MIC_UID "HushCoreMicrophone_UID"
#define RATE 48000.0
#define TONE_HZ 440.0
#define TONE_DBFS (-12.0)
#define RECORD_SECONDS 2.5
#define CLICK_AT_FRAMES 24000u   /* offset from the first sink callback */
#define CLICK_LENGTH 48u
#define CLICK_LEVEL 0.6f
#define CAPTURE_CAPACITY 192000u /* 4 s at 48 kHz */
#define FREQ_TOLERANCE_HZ 5.0
#define LEVEL_TOLERANCE_DB 2.0
#define STABLE_SKIP_FRAMES 4800u /* skip 0.1 s after the click before measuring */
#define MIN_MEASURE_FRAMES 48000u

static struct {
    double sinkBase;
    bool sinkBaseSet;
    double clickSinkTime;
    bool clickWritten;
    uint64_t sinkCallbacks;
    uint64_t micCallbacks;
    uint64_t micGaps;
    double micBase;
    bool micBaseSet;
    float *capture; /* channel 0 of the microphone, index k is sample time micBase + k */
    uint32_t captured;
} g;

static bool gDeliveryOnly = false;

static void Fail(const char *message)
{
    fprintf(stderr, "FAIL: %s\n", message);
}

static OSStatus SinkProc(AudioObjectID inDevice, const AudioTimeStamp *inNow,
                         const AudioBufferList *inInputData, const AudioTimeStamp *inInputTime,
                         AudioBufferList *outOutputData, const AudioTimeStamp *inOutputTime,
                         void *inClientData)
{
    (void)inDevice;
    (void)inNow;
    (void)inInputData;
    (void)inInputTime;
    (void)inClientData;
    g.sinkCallbacks++;
    if (outOutputData == NULL || outOutputData->mNumberBuffers < 1) {
        return noErr;
    }
    float *out = (float *)outOutputData->mBuffers[0].mData;
    if (out == NULL) {
        return noErr;
    }
    uint32_t frames = outOutputData->mBuffers[0].mDataByteSize / (2u * (uint32_t)sizeof(float));
    if (!g.sinkBaseSet) {
        g.sinkBase = inOutputTime->mSampleTime;
        g.sinkBaseSet = true;
    }
    double amplitude = pow(10.0, TONE_DBFS / 20.0);
    double clickStart = g.sinkBase + CLICK_AT_FRAMES;
    for (uint32_t i = 0; i < frames; i++) {
        double t = inOutputTime->mSampleTime + (double)i;
        float sample = (float)(amplitude * sin(2.0 * M_PI * TONE_HZ * (t - g.sinkBase) / RATE));
        if (t >= clickStart && t < clickStart + CLICK_LENGTH) {
            sample += CLICK_LEVEL;
            if (!g.clickWritten) {
                g.clickSinkTime = t;
                g.clickWritten = true;
            }
        }
        out[2 * i] = sample;
        out[2 * i + 1] = sample;
    }
    return noErr;
}

static OSStatus MicProc(AudioObjectID inDevice, const AudioTimeStamp *inNow,
                        const AudioBufferList *inInputData, const AudioTimeStamp *inInputTime,
                        AudioBufferList *outOutputData, const AudioTimeStamp *inOutputTime,
                        void *inClientData)
{
    (void)inDevice;
    (void)inNow;
    (void)outOutputData;
    (void)inOutputTime;
    (void)inClientData;
    g.micCallbacks++;
    if (inInputData == NULL || inInputData->mNumberBuffers < 1) {
        return noErr;
    }
    const float *in = (const float *)inInputData->mBuffers[0].mData;
    if (in == NULL) {
        return noErr;
    }
    uint32_t frames = inInputData->mBuffers[0].mDataByteSize / (2u * (uint32_t)sizeof(float));
    if (!g.micBaseSet) {
        g.micBase = inInputTime->mSampleTime;
        g.micBaseSet = true;
    }
    /* Index from the shared sample timeline; pad with silence if the HAL skipped time. */
    long start = lround(inInputTime->mSampleTime - g.micBase);
    if (start < 0) {
        return noErr;
    }
    if ((uint32_t)start > g.captured) {
        g.micGaps++;
        uint32_t padTo = (uint32_t)start < CAPTURE_CAPACITY ? (uint32_t)start : CAPTURE_CAPACITY;
        memset(&g.capture[g.captured], 0, (padTo - g.captured) * sizeof(float));
        g.captured = padTo;
    }
    for (uint32_t i = 0; i < frames; i++) {
        uint32_t index = (uint32_t)start + i;
        if (index >= CAPTURE_CAPACITY) {
            break;
        }
        g.capture[index] = in[2 * i];
        if (index >= g.captured) {
            g.captured = index + 1u;
        }
    }
    return noErr;
}

static AudioObjectID FindDeviceByUID(const char *uid)
{
    CFStringRef cfUID = CFStringCreateWithCString(kCFAllocatorDefault, uid, kCFStringEncodingUTF8);
    AudioObjectPropertyAddress addr = {
        kAudioHardwarePropertyTranslateUIDToDevice,
        kAudioObjectPropertyScopeGlobal,
        kAudioObjectPropertyElementMain};
    AudioObjectID device = kAudioObjectUnknown;
    UInt32 size = sizeof(device);
    OSStatus err = AudioObjectGetPropertyData(kAudioObjectSystemObject, &addr, sizeof(cfUID), &cfUID,
                                              &size, &device);
    CFRelease(cfUID);
    return err == noErr ? device : kAudioObjectUnknown;
}

static Float64 NominalRate(AudioObjectID device)
{
    AudioObjectPropertyAddress addr = {
        kAudioDevicePropertyNominalSampleRate,
        kAudioObjectPropertyScopeGlobal,
        kAudioObjectPropertyElementMain};
    Float64 rate = 0.0;
    UInt32 size = sizeof(rate);
    if (AudioObjectGetPropertyData(device, &addr, 0, NULL, &size, &rate) != noErr) {
        return 0.0;
    }
    return rate;
}

static UInt32 Latency(AudioObjectID device, AudioObjectPropertyScope scope)
{
    AudioObjectPropertyAddress addr = {kAudioDevicePropertyLatency, scope, kAudioObjectPropertyElementMain};
    UInt32 value = 0;
    UInt32 size = sizeof(value);
    if (AudioObjectGetPropertyData(device, &addr, 0, NULL, &size, &value) != noErr) {
        return 0;
    }
    return value;
}

static void PrintName(AudioObjectID device)
{
    AudioObjectPropertyAddress addr = {kAudioObjectPropertyName, kAudioObjectPropertyScopeGlobal,
                                       kAudioObjectPropertyElementMain};
    CFStringRef name = NULL;
    UInt32 size = sizeof(name);
    char buffer[256] = "?";
    if (AudioObjectGetPropertyData(device, &addr, 0, NULL, &size, &name) == noErr && name != NULL) {
        CFStringGetCString(name, buffer, sizeof(buffer), kCFStringEncodingUTF8);
        CFRelease(name);
    }
    printf("%s", buffer);
}

static int ListDevices(AudioObjectID sink, AudioObjectID mic)
{
    printf("[");
    PrintName(sink);
    printf("] id=%u rate=%.0f Hz outLatency=%u frames\n", (unsigned)sink, NominalRate(sink),
           (unsigned)Latency(sink, kAudioObjectPropertyScopeOutput));
    printf("[");
    PrintName(mic);
    printf("] id=%u rate=%.0f Hz inLatency=%u frames\n", (unsigned)mic, NominalRate(mic),
           (unsigned)Latency(mic, kAudioObjectPropertyScopeInput));
    return 0;
}

static bool SetNominalRate(AudioObjectID device, Float64 rate)
{
    AudioObjectPropertyAddress addr = {
        kAudioDevicePropertyNominalSampleRate,
        kAudioObjectPropertyScopeGlobal,
        kAudioObjectPropertyElementMain};
    return AudioObjectSetPropertyData(device, &addr, 0, NULL, sizeof(rate), &rate) == noErr;
}

static bool WaitForRate(AudioObjectID device, Float64 rate, double seconds)
{
    for (int waited = 0; waited < (int)(seconds * 10); waited++) {
        if (NominalRate(device) == rate) {
            return true;
        }
        usleep(100000);
    }
    return NominalRate(device) == rate;
}

static int Analyze(void)
{
    if (g.sinkCallbacks == 0) {
        Fail("sink IO proc never ran");
        return 1;
    }
    if (g.micCallbacks == 0) {
        Fail("microphone IO proc never ran (no buffers delivered)");
        return 1;
    }
    printf("callbacks: sink=%llu mic=%llu micGaps=%llu frames=%u\n",
           (unsigned long long)g.sinkCallbacks, (unsigned long long)g.micCallbacks,
           (unsigned long long)g.micGaps, (unsigned)g.captured);
    if (gDeliveryOnly) {
        printf("PASS (delivery only): both devices ran and the microphone delivered buffers\n");
        return 0;
    }

    uint32_t firstSignal = 0;
    while (firstSignal < g.captured && fabsf(g.capture[firstSignal]) <= 1e-6f) {
        firstSignal++;
    }
    if (firstSignal >= g.captured) {
        Fail("microphone recorded silence (check microphone permission for this tool)");
        return 1;
    }

    uint32_t clickIndex = 0;
    while (clickIndex < g.captured && fabsf(g.capture[clickIndex]) <= 0.5f) {
        clickIndex++;
    }
    if (clickIndex >= g.captured || !g.clickWritten) {
        Fail("click not found in the microphone recording");
        return 1;
    }
    double latencyFrames = (g.micBase + (double)clickIndex) - g.clickSinkTime;

    uint32_t from = clickIndex + STABLE_SKIP_FRAMES;
    if (from + MIN_MEASURE_FRAMES > g.captured) {
        Fail("recording too short after the click to measure the tone");
        return 1;
    }
    uint32_t count = g.captured - from;
    double sumSquares = 0.0;
    uint32_t positiveCrossings = 0;
    for (uint32_t i = from; i < g.captured; i++) {
        double x = g.capture[i];
        sumSquares += x * x;
        if (i > from && g.capture[i - 1] < 0.0f && g.capture[i] >= 0.0f) {
            positiveCrossings++;
        }
    }
    double rms = sqrt(sumSquares / count);
    double levelDb = 20.0 * log10(rms);
    double expectedDb = 20.0 * log10(pow(10.0, TONE_DBFS / 20.0) / sqrt(2.0));
    double duration = (double)count / RATE;
    double frequency = (double)positiveCrossings / duration;

    printf("loopback latency: %.0f frames (%.2f ms), first signal at mic index %u\n", latencyFrames,
           latencyFrames * 1000.0 / RATE, (unsigned)firstSignal);
    printf("tone level: %.2f dBFS (expected %.2f, tolerance %.1f)\n", levelDb, expectedDb, LEVEL_TOLERANCE_DB);
    printf("tone frequency: %.2f Hz (expected %.0f, tolerance %.1f)\n", frequency, TONE_HZ, FREQ_TOLERANCE_HZ);

    bool ok = true;
    if (fabs(levelDb - expectedDb) > LEVEL_TOLERANCE_DB) {
        Fail("tone level outside tolerance");
        ok = false;
    }
    if (fabs(frequency - TONE_HZ) > FREQ_TOLERANCE_HZ) {
        Fail("tone frequency outside tolerance");
        ok = false;
    }
    if (latencyFrames < 0.0) {
        Fail("click arrived before it was written");
        ok = false;
    }
    if (ok) {
        printf("PASS\n");
    }
    return ok ? 0 : 1;
}

int main(int argc, char **argv)
{
    bool listOnly = false;
    for (int i = 1; i < argc; i++) {
        if (strcmp(argv[i], "--list") == 0) {
            listOnly = true;
        } else if (strcmp(argv[i], "--delivery-only") == 0) {
            gDeliveryOnly = true;
        } else {
            fprintf(stderr, "usage: %s [--list | --delivery-only]\n", argv[0]);
            return 2;
        }
    }

    AudioObjectID sink = FindDeviceByUID(SINK_UID);
    AudioObjectID mic = FindDeviceByUID(MIC_UID);
    if (sink == kAudioObjectUnknown || mic == kAudioObjectUnknown) {
        Fail("HushCore devices not found; install the plug-in first (scripts/macos/install-plugin.sh)");
        return 1;
    }
    if (listOnly) {
        return ListDevices(sink, mic);
    }

    if (!SetNominalRate(sink, RATE) || !WaitForRate(sink, RATE, 5.0) || !WaitForRate(mic, RATE, 5.0)) {
        Fail("could not switch both devices to 48000 Hz");
        return 1;
    }

    g.capture = calloc(CAPTURE_CAPACITY, sizeof(float));
    if (g.capture == NULL) {
        Fail("out of memory");
        return 1;
    }

    AudioDeviceIOProcID sinkProc = NULL;
    AudioDeviceIOProcID micProc = NULL;
    if (AudioDeviceCreateIOProcID(sink, SinkProc, NULL, &sinkProc) != noErr ||
        AudioDeviceCreateIOProcID(mic, MicProc, NULL, &micProc) != noErr) {
        Fail("could not create IO procs");
        free(g.capture);
        return 1;
    }

    if (AudioDeviceStart(sink, sinkProc) != noErr) {
        Fail("could not start HushCore Sink");
        return 1;
    }
    if (AudioDeviceStart(mic, micProc) != noErr) {
        Fail("could not start HushCore Microphone");
        AudioDeviceStop(sink, sinkProc);
        return 1;
    }
    usleep((useconds_t)(RECORD_SECONDS * 1.0e6));
    AudioDeviceStop(mic, micProc);
    AudioDeviceStop(sink, sinkProc);
    AudioDeviceDestroyIOProcID(mic, micProc);
    AudioDeviceDestroyIOProcID(sink, sinkProc);

    int status = Analyze();
    free(g.capture);
    return status;
}
