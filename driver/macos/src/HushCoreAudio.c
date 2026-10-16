/*
 * HushCore virtual audio devices: a Core Audio AudioServerPlugIn loaded by coreaudiod.
 *
 *   HushCore Sink        output only. The HushCore engine plays processed audio into it.
 *   HushCore Microphone  input only. Applications record from it.
 *
 * Both devices share one float32 stereo ring and one host clock. Sample times are the
 * same on both devices, so audio written to the sink at sample time t is read from the
 * microphone at sample time t and the loopback never needs resampling or drift correction.
 */

#include <CoreAudio/CoreAudio.h>
#include <CoreAudio/AudioServerPlugIn.h>
#include <CoreAudio/AudioHardwareBase.h>
#include <CoreFoundation/CoreFoundation.h>
#include <CoreFoundation/CFPlugInCOM.h>
#include <mach/mach_time.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <string.h>
#include <sys/types.h>

#define HC_RING_FRAMES 16384u
#define HC_RING_MASK (HC_RING_FRAMES - 1u)
#define HC_CHANNELS 2u
#define HC_RATE_44100 44100u
#define HC_RATE_48000 48000u
#define HC_IO_BUFFER_FRAMES 512u
/* Small on purpose: the mic lags the sink by about two IO buffers (see README). */
#define HC_SAFETY_OFFSET 32u
#define HC_CLOCK_DOMAIN 0x48435631u

#define HC_MANUFACTURER "HushCore"
#define HC_PLUGIN_NAME "HushCore Audio"
#define HC_SINK_NAME "HushCore Sink"
#define HC_SINK_UID "HushCoreSink_UID"
#define HC_SINK_MODEL_UID "HushCoreSink_ModelUID"
#define HC_SINK_STREAM_NAME "HushCore Sink Output"
#define HC_MIC_NAME "HushCore Microphone"
#define HC_MIC_UID "HushCoreMicrophone_UID"
#define HC_MIC_MODEL_UID "HushCoreMicrophone_ModelUID"
#define HC_MIC_STREAM_NAME "HushCore Microphone Input"

enum {
    kHCObjPlugIn = kAudioObjectPlugInObject,
    kHCObjSinkDevice = 2,
    kHCObjSinkStream = 3,
    kHCObjMicDevice = 4,
    kHCObjMicStream = 5
};

#define HC_DEV_INDEX(obj) ((obj) == kHCObjSinkDevice ? 0u : 1u)

static bool IsDeviceObject(AudioObjectID obj)
{
    return obj == kHCObjSinkDevice || obj == kHCObjMicDevice;
}

static bool IsStreamObject(AudioObjectID obj)
{
    return obj == kHCObjSinkStream || obj == kHCObjMicStream;
}

static bool IsSupportedRate(UInt64 rate)
{
    return rate == HC_RATE_44100 || rate == HC_RATE_48000;
}

/*
 * Loopback ring. Frame f lives in slot f & HC_RING_MASK. The sink publishes gWrittenEnd
 * (one past the newest frame it wrote) with release order after the samples; the
 * microphone loads it with acquire order. Frames at or beyond gWrittenEnd, or more than
 * one ring length behind it, are silence. The sink writes every cycle, so the only
 * remaining race is a microphone read that overlaps the sink overwriting the oldest
 * slots of the ring; that costs at most a click on frames a full ring behind the head.
 */
static float gRing[HC_RING_FRAMES * HC_CHANNELS];
static _Atomic int64_t gWrittenEnd = 0;

static void RingWrite(int64_t start, const float *src, uint32_t frames)
{
    if (start < 0 || frames == 0) {
        return;
    }
    for (uint32_t i = 0; i < frames; i++) {
        uint32_t slot = (uint32_t)((uint64_t)(start + (int64_t)i) & HC_RING_MASK);
        gRing[slot * HC_CHANNELS] = src[i * HC_CHANNELS];
        gRing[slot * HC_CHANNELS + 1u] = src[i * HC_CHANNELS + 1u];
    }
    int64_t end = start + (int64_t)frames;
    if (end > atomic_load_explicit(&gWrittenEnd, memory_order_relaxed)) {
        atomic_store_explicit(&gWrittenEnd, end, memory_order_release);
    }
}

static void RingRead(int64_t start, float *dst, uint32_t frames)
{
    int64_t end = atomic_load_explicit(&gWrittenEnd, memory_order_acquire);
    int64_t oldest = end - (int64_t)HC_RING_FRAMES;
    for (uint32_t i = 0; i < frames; i++) {
        int64_t f = start + (int64_t)i;
        float *out = &dst[i * HC_CHANNELS];
        if (f < 0 || f >= end || f < oldest) {
            out[0] = 0.0f;
            out[1] = 0.0f;
        } else {
            uint32_t slot = (uint32_t)((uint64_t)f & HC_RING_MASK);
            out[0] = gRing[slot * HC_CHANNELS];
            out[1] = gRing[slot * HC_CHANNELS + 1u];
        }
    }
}

/*
 * Clock state read by GetZeroTimeStamp on the IO thread. Two slots plus an atomic index:
 * the writer (always under gLock) fills the inactive slot and then publishes its index with
 * release order. The reader loads the index with acquire order and never waits, so a
 * preempted writer cannot stall the IO thread. A reader that overlaps a second restart can
 * see fields from two restarts; the seed, bumped on every restart, tells clients that the
 * timeline restarted, so they discard that cycle.
 */
typedef struct {
    _Atomic uint64_t anchor;
    _Atomic uint64_t period;
    _Atomic uint64_t seed;
} HCClockSlot;

static HCClockSlot gClockSlots[2];
static _Atomic uint32_t gClockActive = 0;

/* Caller holds gLock. */
static void ClockPublish(uint64_t anchor, uint64_t period, uint64_t seed)
{
    uint32_t next = atomic_load_explicit(&gClockActive, memory_order_relaxed) ^ 1u;
    HCClockSlot *slot = &gClockSlots[next];
    atomic_store_explicit(&slot->anchor, anchor, memory_order_relaxed);
    atomic_store_explicit(&slot->period, period, memory_order_relaxed);
    atomic_store_explicit(&slot->seed, seed, memory_order_relaxed);
    atomic_store_explicit(&gClockActive, next, memory_order_release);
}

static void ClockRead(uint64_t *anchor, uint64_t *period, uint64_t *seed)
{
    uint32_t active = atomic_load_explicit(&gClockActive, memory_order_acquire);
    const HCClockSlot *slot = &gClockSlots[active];
    *anchor = atomic_load_explicit(&slot->anchor, memory_order_relaxed);
    *period = atomic_load_explicit(&slot->period, memory_order_relaxed);
    *seed = atomic_load_explicit(&slot->seed, memory_order_relaxed);
}

/* Protects all control-plane state. Never taken on the IO path. */
static pthread_mutex_t gLock = PTHREAD_MUTEX_INITIALIZER;
static AudioServerPlugInHostRef gHost = NULL;
static UInt32 gRate = HC_RATE_48000;
static UInt64 gSeed = 1;
static UInt32 gDeviceClients[2] = {0, 0};
static UInt32 gRunningDevices = 0;

/* Host ticks in one ring period (ZeroTimeStampPeriod) at the given sample rate. */
static uint64_t HostTicksPerRingPeriod(UInt32 rate)
{
    mach_timebase_info_data_t tb;
    (void)mach_timebase_info(&tb);
    double ns = (double)HC_RING_FRAMES * 1.0e9 / (double)rate;
    return (uint64_t)(ns * (double)tb.denom / (double)tb.numer + 0.5);
}

/*
 * Caller holds gLock. Restarts the shared clock so both devices count from sample 0. The seed
 * changes on every restart, so clients see the timeline restart, including after a rate change.
 */
static void ClockRestartLocked(void)
{
    gSeed++;
    ClockPublish(mach_absolute_time(), HostTicksPerRingPeriod(gRate), gSeed);
}

/* Property values are built into a small buffer so size queries and reads share one code path. */
typedef struct {
    UInt32 size;
    uint8_t bytes[512];
} HCValue;

static void ValPut(HCValue *v, const void *src, UInt32 n)
{
    memcpy(v->bytes, src, n);
    v->size = n;
}

static void ValU32(HCValue *v, UInt32 x)
{
    ValPut(v, &x, sizeof(x));
}

static void ValF64(HCValue *v, Float64 x)
{
    ValPut(v, &x, sizeof(x));
}

static void ValObj(HCValue *v, AudioObjectID x)
{
    ValPut(v, &x, sizeof(x));
}

static void ValObjs(HCValue *v, const AudioObjectID *ids, UInt32 n)
{
    if (n == 0) {
        v->size = 0;
        return;
    }
    ValPut(v, ids, n * (UInt32)sizeof(AudioObjectID));
}

/*
 * Strings are returned retained (the Copy rule), so a CFString is only created when the
 * caller will actually consume the value (build == true). Size queries pass build == false.
 */
static void ValString(HCValue *v, bool build, const char *s)
{
    v->size = (UInt32)sizeof(CFStringRef);
    if (build) {
        CFStringRef str = CFStringCreateWithCString(kCFAllocatorDefault, s, kCFStringEncodingUTF8);
        ValPut(v, &str, (UInt32)sizeof(str));
    }
}

static AudioStreamBasicDescription MakeFormat(Float64 rate)
{
    AudioStreamBasicDescription f;
    memset(&f, 0, sizeof(f));
    f.mSampleRate = rate;
    f.mFormatID = kAudioFormatLinearPCM;
    f.mFormatFlags = kAudioFormatFlagsNativeFloatPacked;
    f.mBytesPerPacket = HC_CHANNELS * sizeof(float);
    f.mFramesPerPacket = 1;
    f.mBytesPerFrame = HC_CHANNELS * sizeof(float);
    f.mChannelsPerFrame = HC_CHANNELS;
    f.mBitsPerChannel = 32;
    return f;
}

static AudioObjectID FindDeviceByUID(CFStringRef uid)
{
    if (uid == NULL) {
        return kAudioObjectUnknown;
    }
    if (CFStringCompare(uid, CFSTR(HC_SINK_UID), 0) == kCFCompareEqualTo) {
        return kHCObjSinkDevice;
    }
    if (CFStringCompare(uid, CFSTR(HC_MIC_UID), 0) == kCFCompareEqualTo) {
        return kHCObjMicDevice;
    }
    return kAudioObjectUnknown;
}

static OSStatus UIDQualifier(UInt32 qualifierSize, const void *qualifier, CFStringRef *uid)
{
    if (qualifierSize != sizeof(CFStringRef) || qualifier == NULL) {
        return kAudioHardwareBadPropertySizeError;
    }
    *uid = *(CFStringRef const *)qualifier;
    return kAudioHardwareNoError;
}

static OSStatus DescribePlugIn(const AudioObjectPropertyAddress *a, UInt32 qualifierSize,
                               const void *qualifier, bool build, HCValue *v)
{
    AudioObjectID devices[2] = {kHCObjSinkDevice, kHCObjMicDevice};

    switch (a->mSelector) {
    case kAudioObjectPropertyBaseClass:
        ValU32(v, kAudioObjectClassID);
        return kAudioHardwareNoError;
    case kAudioObjectPropertyClass:
        ValU32(v, kAudioPlugInClassID);
        return kAudioHardwareNoError;
    case kAudioObjectPropertyOwner:
        ValObj(v, kAudioObjectUnknown);
        return kAudioHardwareNoError;
    case kAudioObjectPropertyManufacturer:
        ValString(v, build, HC_MANUFACTURER);
        return kAudioHardwareNoError;
    case kAudioObjectPropertyName:
        ValString(v, build, HC_PLUGIN_NAME);
        return kAudioHardwareNoError;
    case kAudioObjectPropertyOwnedObjects:
    case kAudioPlugInPropertyDeviceList:
        ValObjs(v, devices, 2);
        return kAudioHardwareNoError;
    case kAudioObjectPropertyControlList:
    case kAudioPlugInPropertyBoxList:
        ValObjs(v, NULL, 0);
        return kAudioHardwareNoError;
    case kAudioPlugInPropertyTranslateUIDToBox:
        ValObj(v, kAudioObjectUnknown);
        return kAudioHardwareNoError;
    case kAudioPlugInPropertyTranslateUIDToDevice: {
        if (!build) {
            ValObj(v, kAudioObjectUnknown);
            return kAudioHardwareNoError;
        }
        CFStringRef uid = NULL;
        OSStatus err = UIDQualifier(qualifierSize, qualifier, &uid);
        if (err != kAudioHardwareNoError) {
            return err;
        }
        ValObj(v, FindDeviceByUID(uid));
        return kAudioHardwareNoError;
    }
    case kAudioPlugInPropertyResourceBundle:
        ValString(v, build, "");
        return kAudioHardwareNoError;
    default:
        return kAudioHardwareUnknownPropertyError;
    }
}

static OSStatus DescribeDevice(AudioObjectID obj, const AudioObjectPropertyAddress *a, bool build, HCValue *v)
{
    bool isSink = obj == kHCObjSinkDevice;
    AudioObjectID stream = isSink ? kHCObjSinkStream : kHCObjMicStream;
    AudioObjectID bothDevices[2] = {kHCObjSinkDevice, kHCObjMicDevice};
    AudioObjectID streamList[1] = {stream};
    bool scopeMatches = a->mScope == kAudioObjectPropertyScopeGlobal ||
                        (isSink ? a->mScope == kAudioObjectPropertyScopeOutput
                                : a->mScope == kAudioObjectPropertyScopeInput);
    bool canBeDefault = !isSink && a->mScope != kAudioObjectPropertyScopeOutput;

    switch (a->mSelector) {
    case kAudioObjectPropertyBaseClass:
        ValU32(v, kAudioObjectClassID);
        return kAudioHardwareNoError;
    case kAudioObjectPropertyClass:
        ValU32(v, kAudioDeviceClassID);
        return kAudioHardwareNoError;
    case kAudioObjectPropertyOwner:
        ValObj(v, kHCObjPlugIn);
        return kAudioHardwareNoError;
    case kAudioObjectPropertyName:
        ValString(v, build, isSink ? HC_SINK_NAME : HC_MIC_NAME);
        return kAudioHardwareNoError;
    case kAudioObjectPropertyManufacturer:
        ValString(v, build, HC_MANUFACTURER);
        return kAudioHardwareNoError;
    case kAudioObjectPropertyOwnedObjects:
        ValObjs(v, streamList, scopeMatches ? 1u : 0u);
        return kAudioHardwareNoError;
    case kAudioObjectPropertyControlList:
        ValObjs(v, NULL, 0);
        return kAudioHardwareNoError;
    case kAudioDevicePropertyDeviceUID:
        ValString(v, build, isSink ? HC_SINK_UID : HC_MIC_UID);
        return kAudioHardwareNoError;
    case kAudioDevicePropertyModelUID:
        ValString(v, build, isSink ? HC_SINK_MODEL_UID : HC_MIC_MODEL_UID);
        return kAudioHardwareNoError;
    case kAudioDevicePropertyTransportType:
        ValU32(v, kAudioDeviceTransportTypeVirtual);
        return kAudioHardwareNoError;
    case kAudioDevicePropertyRelatedDevices:
        ValObjs(v, bothDevices, 2);
        return kAudioHardwareNoError;
    case kAudioDevicePropertyClockDomain:
        ValU32(v, HC_CLOCK_DOMAIN);
        return kAudioHardwareNoError;
    case kAudioDevicePropertyDeviceIsAlive:
        ValU32(v, 1);
        return kAudioHardwareNoError;
    case kAudioDevicePropertyDeviceIsRunning:
    case kAudioDevicePropertyDeviceIsRunningSomewhere:
        ValU32(v, gDeviceClients[HC_DEV_INDEX(obj)] > 0 ? 1u : 0u);
        return kAudioHardwareNoError;
    case kAudioDevicePropertyDeviceCanBeDefaultDevice:
        ValU32(v, canBeDefault ? 1u : 0u);
        return kAudioHardwareNoError;
    case kAudioDevicePropertyDeviceCanBeDefaultSystemDevice:
        ValU32(v, 0);
        return kAudioHardwareNoError;
    case kAudioDevicePropertyLatency:
        ValU32(v, 0);
        return kAudioHardwareNoError;
    case kAudioDevicePropertyStreams:
        ValObjs(v, streamList, scopeMatches ? 1u : 0u);
        return kAudioHardwareNoError;
    case kAudioDevicePropertySafetyOffset:
        ValU32(v, HC_SAFETY_OFFSET);
        return kAudioHardwareNoError;
    case kAudioDevicePropertyNominalSampleRate:
        ValF64(v, (Float64)gRate);
        return kAudioHardwareNoError;
    case kAudioDevicePropertyAvailableNominalSampleRates: {
        AudioValueRange rates[2] = {
            {(Float64)HC_RATE_44100, (Float64)HC_RATE_44100},
            {(Float64)HC_RATE_48000, (Float64)HC_RATE_48000}};
        ValPut(v, rates, (UInt32)sizeof(rates));
        return kAudioHardwareNoError;
    }
    case kAudioDevicePropertyIsHidden:
        ValU32(v, 0);
        return kAudioHardwareNoError;
    case kAudioDevicePropertyPreferredChannelsForStereo: {
        UInt32 channels[2] = {1, 2};
        ValPut(v, channels, (UInt32)sizeof(channels));
        return kAudioHardwareNoError;
    }
    case kAudioDevicePropertyZeroTimeStampPeriod:
        ValU32(v, HC_RING_FRAMES);
        return kAudioHardwareNoError;
    case kAudioDevicePropertyClockIsStable:
        ValU32(v, 1);
        return kAudioHardwareNoError;
    case kAudioDevicePropertyBufferFrameSize:
        ValU32(v, HC_IO_BUFFER_FRAMES);
        return kAudioHardwareNoError;
    case kAudioDevicePropertyBufferFrameSizeRange: {
        AudioValueRange range = {(Float64)HC_IO_BUFFER_FRAMES, (Float64)HC_IO_BUFFER_FRAMES};
        ValPut(v, &range, (UInt32)sizeof(range));
        return kAudioHardwareNoError;
    }
    case kAudioDevicePropertyStreamConfiguration: {
        AudioBufferList list;
        memset(&list, 0, sizeof(list));
        list.mNumberBuffers = scopeMatches ? 1u : 0u;
        list.mBuffers[0].mNumberChannels = HC_CHANNELS;
        ValPut(v, &list, scopeMatches ? (UInt32)sizeof(list) : (UInt32)offsetof(AudioBufferList, mBuffers));
        return kAudioHardwareNoError;
    }
    default:
        return kAudioHardwareUnknownPropertyError;
    }
}

static OSStatus DescribeStream(AudioObjectID obj, const AudioObjectPropertyAddress *a, bool build, HCValue *v)
{
    bool isInput = obj == kHCObjMicStream;
    AudioObjectID device = isInput ? kHCObjMicDevice : kHCObjSinkDevice;

    switch (a->mSelector) {
    case kAudioObjectPropertyBaseClass:
        ValU32(v, kAudioObjectClassID);
        return kAudioHardwareNoError;
    case kAudioObjectPropertyClass:
        ValU32(v, kAudioStreamClassID);
        return kAudioHardwareNoError;
    case kAudioObjectPropertyOwner:
        ValObj(v, device);
        return kAudioHardwareNoError;
    case kAudioObjectPropertyName:
        ValString(v, build, isInput ? HC_MIC_STREAM_NAME : HC_SINK_STREAM_NAME);
        return kAudioHardwareNoError;
    case kAudioObjectPropertyManufacturer:
        ValString(v, build, HC_MANUFACTURER);
        return kAudioHardwareNoError;
    case kAudioObjectPropertyOwnedObjects:
        ValObjs(v, NULL, 0);
        return kAudioHardwareNoError;
    case kAudioStreamPropertyIsActive:
        ValU32(v, 1);
        return kAudioHardwareNoError;
    case kAudioStreamPropertyDirection:
        ValU32(v, isInput ? 1u : 0u);
        return kAudioHardwareNoError;
    case kAudioStreamPropertyTerminalType:
        ValU32(v, isInput ? kAudioStreamTerminalTypeMicrophone : kAudioStreamTerminalTypeSpeaker);
        return kAudioHardwareNoError;
    case kAudioStreamPropertyStartingChannel:
        ValU32(v, 1);
        return kAudioHardwareNoError;
    case kAudioStreamPropertyLatency:
        ValU32(v, 0);
        return kAudioHardwareNoError;
    case kAudioStreamPropertyVirtualFormat:
    case kAudioStreamPropertyPhysicalFormat: {
        AudioStreamBasicDescription format = MakeFormat((Float64)gRate);
        ValPut(v, &format, (UInt32)sizeof(format));
        return kAudioHardwareNoError;
    }
    case kAudioStreamPropertyAvailableVirtualFormats:
    case kAudioStreamPropertyAvailablePhysicalFormats: {
        AudioStreamRangedDescription ranged;
        memset(&ranged, 0, sizeof(ranged));
        ranged.mFormat = MakeFormat((Float64)gRate);
        ranged.mSampleRateRange.mMinimum = (Float64)HC_RATE_44100;
        ranged.mSampleRateRange.mMaximum = (Float64)HC_RATE_48000;
        ValPut(v, &ranged, (UInt32)sizeof(ranged));
        return kAudioHardwareNoError;
    }
    default:
        return kAudioHardwareUnknownPropertyError;
    }
}

/* Single description of every object and property. Caller holds gLock. */
static OSStatus Describe(AudioObjectID obj, const AudioObjectPropertyAddress *a,
                         UInt32 qualifierSize, const void *qualifier, bool build, HCValue *v)
{
    v->size = 0;
    if (obj == kHCObjPlugIn) {
        return DescribePlugIn(a, qualifierSize, qualifier, build, v);
    }
    if (IsDeviceObject(obj)) {
        return DescribeDevice(obj, a, build, v);
    }
    if (IsStreamObject(obj)) {
        return DescribeStream(obj, a, build, v);
    }
    return kAudioHardwareBadObjectError;
}

/* ---- IUnknown ---------------------------------------------------------- */

static HRESULT DriverQueryInterface(void *inDriver, REFIID inUUID, LPVOID *outInterface)
{
    if (outInterface == NULL) {
        return (HRESULT)0x80000003L;
    }
    *outInterface = NULL;
    /* Only the driver interface and IUnknown are served; anything else is E_NOINTERFACE (0x80000004). */
    CFUUIDRef requested = CFUUIDCreateFromUUIDBytes(kCFAllocatorDefault, inUUID);
    if (requested == NULL) {
        return (HRESULT)0x80000004L;
    }
    Boolean matches = CFEqual(requested, kAudioServerPlugInDriverInterfaceUUID) ||
                      CFEqual(requested, IUnknownUUID);
    CFRelease(requested);
    if (!matches) {
        return (HRESULT)0x80000004L;
    }
    *outInterface = inDriver;
    return (HRESULT)0;
}

static ULONG DriverAddRef(void *inDriver)
{
    (void)inDriver;
    return 1;
}

static ULONG DriverRelease(void *inDriver)
{
    (void)inDriver;
    return 1;
}

/* ---- Plug-in lifecycle ------------------------------------------------- */

static OSStatus DriverInitialize(AudioServerPlugInDriverRef inDriver, AudioServerPlugInHostRef inHost)
{
    (void)inDriver;
    pthread_mutex_lock(&gLock);
    gHost = inHost;
    if (gRunningDevices == 0) {
        ClockRestartLocked();
    }
    pthread_mutex_unlock(&gLock);
    return kAudioHardwareNoError;
}

static OSStatus DriverCreateDevice(AudioServerPlugInDriverRef inDriver, CFDictionaryRef inDescription,
                                   const AudioServerPlugInClientInfo *inClientInfo,
                                   AudioObjectID *outDeviceObjectID)
{
    (void)inDriver;
    (void)inDescription;
    (void)inClientInfo;
    (void)outDeviceObjectID;
    return kAudioHardwareUnsupportedOperationError;
}

static OSStatus DriverDestroyDevice(AudioServerPlugInDriverRef inDriver, AudioObjectID inDeviceObjectID)
{
    (void)inDriver;
    (void)inDeviceObjectID;
    return kAudioHardwareUnsupportedOperationError;
}

static OSStatus DriverAddDeviceClient(AudioServerPlugInDriverRef inDriver, AudioObjectID inDeviceObjectID,
                                      const AudioServerPlugInClientInfo *inClientInfo)
{
    (void)inDriver;
    (void)inClientInfo;
    return IsDeviceObject(inDeviceObjectID) ? kAudioHardwareNoError : kAudioHardwareBadObjectError;
}

static OSStatus DriverRemoveDeviceClient(AudioServerPlugInDriverRef inDriver, AudioObjectID inDeviceObjectID,
                                         const AudioServerPlugInClientInfo *inClientInfo)
{
    (void)inDriver;
    (void)inClientInfo;
    return IsDeviceObject(inDeviceObjectID) ? kAudioHardwareNoError : kAudioHardwareBadObjectError;
}

static void NotifyChanged(AudioServerPlugInHostRef host, AudioObjectID obj, AudioObjectPropertySelector selector)
{
    AudioObjectPropertyAddress addr = {selector, kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyElementMain};
    (void)(*host)->PropertiesChanged(host, obj, 1, &addr);
}

/*
 * The host calls this after RequestDeviceConfigurationChange, with the requested rate as
 * the action. Both devices move together, so the loopback never resamples.
 */
static OSStatus DriverPerformDeviceConfigurationChange(AudioServerPlugInDriverRef inDriver,
                                                       AudioObjectID inDeviceObjectID,
                                                       UInt64 inChangeAction, void *inChangeInfo)
{
    (void)inDriver;
    (void)inChangeInfo;
    if (!IsDeviceObject(inDeviceObjectID)) {
        return kAudioHardwareBadObjectError;
    }
    if (!IsSupportedRate(inChangeAction)) {
        return kAudioDeviceUnsupportedFormatError;
    }

    pthread_mutex_lock(&gLock);
    AudioServerPlugInHostRef host = gHost;
    gRate = (UInt32)inChangeAction;
    /* A rate change while running restarts the sample timeline, so the ring restarts too. */
    atomic_store_explicit(&gWrittenEnd, 0, memory_order_release);
    ClockRestartLocked();
    pthread_mutex_unlock(&gLock);

    if (host != NULL) {
        NotifyChanged(host, kHCObjSinkDevice, kAudioDevicePropertyNominalSampleRate);
        NotifyChanged(host, kHCObjMicDevice, kAudioDevicePropertyNominalSampleRate);
        NotifyChanged(host, kHCObjSinkStream, kAudioStreamPropertyVirtualFormat);
        NotifyChanged(host, kHCObjSinkStream, kAudioStreamPropertyPhysicalFormat);
        NotifyChanged(host, kHCObjMicStream, kAudioStreamPropertyVirtualFormat);
        NotifyChanged(host, kHCObjMicStream, kAudioStreamPropertyPhysicalFormat);
    }
    return kAudioHardwareNoError;
}

static OSStatus DriverAbortDeviceConfigurationChange(AudioServerPlugInDriverRef inDriver,
                                                     AudioObjectID inDeviceObjectID,
                                                     UInt64 inChangeAction, void *inChangeInfo)
{
    (void)inDriver;
    (void)inDeviceObjectID;
    (void)inChangeAction;
    (void)inChangeInfo;
    return kAudioHardwareNoError;
}

/* ---- Properties -------------------------------------------------------- */

static Boolean DriverHasProperty(AudioServerPlugInDriverRef inDriver, AudioObjectID inObjectID,
                                 pid_t inClientProcessID, const AudioObjectPropertyAddress *inAddress)
{
    (void)inDriver;
    (void)inClientProcessID;
    HCValue v;
    pthread_mutex_lock(&gLock);
    OSStatus err = Describe(inObjectID, inAddress, 0, NULL, false, &v);
    pthread_mutex_unlock(&gLock);
    return err == kAudioHardwareNoError;
}

static OSStatus DriverIsPropertySettable(AudioServerPlugInDriverRef inDriver, AudioObjectID inObjectID,
                                         pid_t inClientProcessID, const AudioObjectPropertyAddress *inAddress,
                                         Boolean *outIsSettable)
{
    (void)inDriver;
    (void)inClientProcessID;
    HCValue v;
    pthread_mutex_lock(&gLock);
    OSStatus err = Describe(inObjectID, inAddress, 0, NULL, false, &v);
    pthread_mutex_unlock(&gLock);
    if (err != kAudioHardwareNoError) {
        return err;
    }
    *outIsSettable = (IsDeviceObject(inObjectID) &&
                      inAddress->mSelector == kAudioDevicePropertyNominalSampleRate) ? 1 : 0;
    return kAudioHardwareNoError;
}

static OSStatus DriverGetPropertyDataSize(AudioServerPlugInDriverRef inDriver, AudioObjectID inObjectID,
                                          pid_t inClientProcessID, const AudioObjectPropertyAddress *inAddress,
                                          UInt32 inQualifierDataSize, const void *inQualifierData,
                                          UInt32 *outDataSize)
{
    (void)inDriver;
    (void)inClientProcessID;
    HCValue v;
    pthread_mutex_lock(&gLock);
    OSStatus err = Describe(inObjectID, inAddress, inQualifierDataSize, inQualifierData, false, &v);
    pthread_mutex_unlock(&gLock);
    if (err == kAudioHardwareNoError) {
        *outDataSize = v.size;
    }
    return err;
}

static OSStatus DriverGetPropertyData(AudioServerPlugInDriverRef inDriver, AudioObjectID inObjectID,
                                      pid_t inClientProcessID, const AudioObjectPropertyAddress *inAddress,
                                      UInt32 inQualifierDataSize, const void *inQualifierData,
                                      UInt32 inDataSize, UInt32 *outDataSize, void *outData)
{
    (void)inDriver;
    (void)inClientProcessID;
    HCValue size;
    HCValue v;
    pthread_mutex_lock(&gLock);
    /* Size is checked before the value is built, so a rejected read never creates a CFString to leak. */
    OSStatus err = Describe(inObjectID, inAddress, inQualifierDataSize, inQualifierData, false, &size);
    if (err == kAudioHardwareNoError) {
        if (inDataSize < size.size) {
            err = kAudioHardwareBadPropertySizeError;
        } else {
            err = Describe(inObjectID, inAddress, inQualifierDataSize, inQualifierData, true, &v);
        }
    }
    pthread_mutex_unlock(&gLock);
    if (err != kAudioHardwareNoError) {
        return err;
    }
    if (v.size > 0) {
        memcpy(outData, v.bytes, v.size);
    }
    *outDataSize = v.size;
    return kAudioHardwareNoError;
}

static OSStatus DriverSetPropertyData(AudioServerPlugInDriverRef inDriver, AudioObjectID inObjectID,
                                      pid_t inClientProcessID, const AudioObjectPropertyAddress *inAddress,
                                      UInt32 inQualifierDataSize, const void *inQualifierData,
                                      UInt32 inDataSize, const void *inData)
{
    (void)inDriver;
    (void)inClientProcessID;
    (void)inQualifierDataSize;
    (void)inQualifierData;
    if (!IsDeviceObject(inObjectID)) {
        return kAudioHardwareBadObjectError;
    }
    if (inAddress->mSelector != kAudioDevicePropertyNominalSampleRate) {
        return kAudioHardwareUnknownPropertyError;
    }
    if (inDataSize != sizeof(Float64)) {
        return kAudioHardwareBadPropertySizeError;
    }
    Float64 requested;
    memcpy(&requested, inData, sizeof(requested));
    if (requested != (Float64)HC_RATE_44100 && requested != (Float64)HC_RATE_48000) {
        return kAudioDeviceUnsupportedFormatError;
    }
    UInt64 rate = (UInt64)requested;

    pthread_mutex_lock(&gLock);
    AudioServerPlugInHostRef host = gHost;
    UInt32 current = gRate;
    pthread_mutex_unlock(&gLock);

    if (host == NULL) {
        return kAudioHardwareIllegalOperationError;
    }
    if (rate == current) {
        return kAudioHardwareNoError;
    }
    /* The host calls PerformDeviceConfigurationChange once the change is accepted. */
    return (*host)->RequestDeviceConfigurationChange(host, inObjectID, rate, NULL);
}

/* ---- IO control and clock --------------------------------------------- */

static OSStatus DriverStartIO(AudioServerPlugInDriverRef inDriver, AudioObjectID inDeviceObjectID, UInt32 inClientID)
{
    (void)inDriver;
    (void)inClientID;
    if (!IsDeviceObject(inDeviceObjectID)) {
        return kAudioHardwareBadObjectError;
    }
    pthread_mutex_lock(&gLock);
    if (gRunningDevices == 0) {
        memset(gRing, 0, sizeof(gRing));
        atomic_store_explicit(&gWrittenEnd, 0, memory_order_release);
        ClockRestartLocked();
    }
    if (gDeviceClients[HC_DEV_INDEX(inDeviceObjectID)]++ == 0) {
        gRunningDevices++;
    }
    pthread_mutex_unlock(&gLock);
    return kAudioHardwareNoError;
}

static OSStatus DriverStopIO(AudioServerPlugInDriverRef inDriver, AudioObjectID inDeviceObjectID, UInt32 inClientID)
{
    (void)inDriver;
    (void)inClientID;
    if (!IsDeviceObject(inDeviceObjectID)) {
        return kAudioHardwareBadObjectError;
    }
    pthread_mutex_lock(&gLock);
    UInt32 *clients = &gDeviceClients[HC_DEV_INDEX(inDeviceObjectID)];
    if (*clients > 0 && --(*clients) == 0) {
        gRunningDevices--;
    }
    if (gRunningDevices == 0) {
        atomic_store_explicit(&gWrittenEnd, 0, memory_order_release);
    }
    pthread_mutex_unlock(&gLock);
    return kAudioHardwareNoError;
}

static OSStatus DriverGetZeroTimeStamp(AudioServerPlugInDriverRef inDriver, AudioObjectID inDeviceObjectID,
                                       UInt32 inClientID, Float64 *outSampleTime, UInt64 *outHostTime,
                                       UInt64 *outSeed)
{
    (void)inDriver;
    (void)inClientID;
    if (!IsDeviceObject(inDeviceObjectID)) {
        return kAudioHardwareBadDeviceError;
    }
    uint64_t anchor;
    uint64_t period;
    uint64_t seed;
    ClockRead(&anchor, &period, &seed);
    if (period == 0) {
        return kAudioHardwareNotRunningError;
    }
    uint64_t now = mach_absolute_time();
    if (now < anchor) {
        now = anchor;
    }
    uint64_t k = (now - anchor) / period;
    *outHostTime = anchor + k * period;
    *outSampleTime = (Float64)(k * HC_RING_FRAMES);
    *outSeed = seed;
    return kAudioHardwareNoError;
}

static OSStatus DriverWillDoIOOperation(AudioServerPlugInDriverRef inDriver, AudioObjectID inDeviceObjectID,
                                        UInt32 inClientID, UInt32 inOperationID,
                                        Boolean *outWillDo, Boolean *outWillDoInPlace)
{
    (void)inDriver;
    (void)inClientID;
    bool sinkWrite = inDeviceObjectID == kHCObjSinkDevice && inOperationID == kAudioServerPlugInIOOperationWriteMix;
    bool micRead = inDeviceObjectID == kHCObjMicDevice && inOperationID == kAudioServerPlugInIOOperationReadInput;
    *outWillDo = (sinkWrite || micRead) ? 1 : 0;
    *outWillDoInPlace = 1;
    return kAudioHardwareNoError;
}

static OSStatus DriverBeginIOOperation(AudioServerPlugInDriverRef inDriver, AudioObjectID inDeviceObjectID,
                                       UInt32 inClientID, UInt32 inOperationID, UInt32 inIOBufferFrameSize,
                                       const AudioServerPlugInIOCycleInfo *inIOCycleInfo)
{
    (void)inDriver;
    (void)inDeviceObjectID;
    (void)inClientID;
    (void)inOperationID;
    (void)inIOBufferFrameSize;
    (void)inIOCycleInfo;
    return kAudioHardwareNoError;
}

/* Real-time: no allocation, no locks, no logging. */
static OSStatus DriverDoIOOperation(AudioServerPlugInDriverRef inDriver, AudioObjectID inDeviceObjectID,
                                    AudioObjectID inStreamObjectID, UInt32 inClientID, UInt32 inOperationID,
                                    UInt32 inIOBufferFrameSize, const AudioServerPlugInIOCycleInfo *inIOCycleInfo,
                                    void *ioMainBuffer, void *ioSecondaryBuffer)
{
    (void)inDriver;
    (void)inStreamObjectID;
    (void)inClientID;
    (void)ioSecondaryBuffer;
    if (inIOCycleInfo == NULL || ioMainBuffer == NULL) {
        return kAudioHardwareNoError;
    }
    if (inDeviceObjectID == kHCObjSinkDevice && inOperationID == kAudioServerPlugInIOOperationWriteMix) {
        RingWrite((int64_t)(inIOCycleInfo->mOutputTime.mSampleTime + 0.5), (const float *)ioMainBuffer,
                  inIOBufferFrameSize);
    } else if (inDeviceObjectID == kHCObjMicDevice && inOperationID == kAudioServerPlugInIOOperationReadInput) {
        RingRead((int64_t)(inIOCycleInfo->mInputTime.mSampleTime + 0.5), (float *)ioMainBuffer,
                 inIOBufferFrameSize);
    }
    return kAudioHardwareNoError;
}

static OSStatus DriverEndIOOperation(AudioServerPlugInDriverRef inDriver, AudioObjectID inDeviceObjectID,
                                     UInt32 inClientID, UInt32 inOperationID, UInt32 inIOBufferFrameSize,
                                     const AudioServerPlugInIOCycleInfo *inIOCycleInfo)
{
    (void)inDriver;
    (void)inDeviceObjectID;
    (void)inClientID;
    (void)inOperationID;
    (void)inIOBufferFrameSize;
    (void)inIOCycleInfo;
    return kAudioHardwareNoError;
}

/* ---- Vtable and factory ------------------------------------------------ */

static AudioServerPlugInDriverInterface gDriverVTable = {
    ._reserved = NULL,
    .QueryInterface = DriverQueryInterface,
    .AddRef = DriverAddRef,
    .Release = DriverRelease,
    .Initialize = DriverInitialize,
    .CreateDevice = DriverCreateDevice,
    .DestroyDevice = DriverDestroyDevice,
    .AddDeviceClient = DriverAddDeviceClient,
    .RemoveDeviceClient = DriverRemoveDeviceClient,
    .PerformDeviceConfigurationChange = DriverPerformDeviceConfigurationChange,
    .AbortDeviceConfigurationChange = DriverAbortDeviceConfigurationChange,
    .HasProperty = DriverHasProperty,
    .IsPropertySettable = DriverIsPropertySettable,
    .GetPropertyDataSize = DriverGetPropertyDataSize,
    .GetPropertyData = DriverGetPropertyData,
    .SetPropertyData = DriverSetPropertyData,
    .StartIO = DriverStartIO,
    .StopIO = DriverStopIO,
    .GetZeroTimeStamp = DriverGetZeroTimeStamp,
    .WillDoIOOperation = DriverWillDoIOOperation,
    .BeginIOOperation = DriverBeginIOOperation,
    .DoIOOperation = DriverDoIOOperation,
    .EndIOOperation = DriverEndIOOperation,
};

static AudioServerPlugInDriverInterface *gDriverRef = &gDriverVTable;

/* Factory named by CFPlugInFactories in Info.plist. */
void *HushCore_Create(CFAllocatorRef inAllocator, CFUUIDRef inRequestedTypeUUID)
{
    (void)inAllocator;
    if (inRequestedTypeUUID == NULL) {
        return NULL;
    }
    CFStringRef requested = CFUUIDCreateString(kCFAllocatorDefault, inRequestedTypeUUID);
    if (requested == NULL) {
        return NULL;
    }
    Boolean matches = CFStringCompare(requested, CFSTR("443ABAB8-E7B3-491A-B985-BEB9187030DB"), 0) ==
                      kCFCompareEqualTo;
    CFRelease(requested);
    return matches ? (void *)&gDriverRef : NULL;
}
