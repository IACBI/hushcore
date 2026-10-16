<a id="top"></a>
# HushCore

Local, privacy-first real-time microphone processor for Windows, macOS and Linux. Noise suppression, a studio FX rack and its own virtual microphone, all on your own CPU.

![License: MIT](https://img.shields.io/badge/license-MIT-green)
![Python 3.10+](https://img.shields.io/badge/python-3.10%2B-blue)
![Platforms: Windows | macOS | Linux](https://img.shields.io/badge/platform-Windows%20%7C%20macOS%20%7C%20Linux-lightgrey)

**Read this in:** [English](#english) · [Türkçe](#turkce)

---
<a id="english"></a>
## English

### Overview

HushCore takes your microphone, cleans it up in real time and publishes the result as its own virtual microphone, **HushCore Microphone**. Discord, OBS, Zoom and other apps pick that microphone and receive the processed voice instead of the raw input. No third-party virtual audio software is needed: each platform uses HushCore's own component built on the operating system's audio APIs (a WaveRT driver on Windows, a Core Audio plug-in on macOS, a PipeWire or PulseAudio virtual device on Linux).

Everything runs on your machine. Audio is processed frame by frame on the CPU and is never sent anywhere. The dashboard and API listen on `127.0.0.1` only, and each start generates a new API key that the dashboard receives automatically.

### Features

- **Own virtual microphone** on Windows, macOS and Linux, installed and removed with one script per platform. See [docs/VIRTUAL_DEVICE.md](docs/VIRTUAL_DEVICE.md).
- **Noise suppression** in three modes: `OFF`, `ECO DSP` (spectral subtraction, no AI dependencies) and `HQ AI` (DeepFilterNet, optional).
- **Studio FX rack**: noise gate, 3-band EQ, compressor, de-esser, harmonic vocal exciter and peak limiter.
- **VST3 host**: load any `.vst3` plugin through [pedalboard](https://github.com/spotify/pedalboard) and open its own editor from the dashboard.
- **Presets**: four built-in presets, plus your own saved to `backend/user_presets.json`.
- **Native audio engine**: a real-time Rust engine (WASAPI on Windows, Core Audio on macOS, PulseAudio/PipeWire on Linux, about 1% CPU) runs the chain when it is built, and the Python engine takes over for VST3 or when the native one is missing. On our test PC (Realtek, shared mode limited to 10 ms) end-to-end latency was about 35 ms shared and 17 ms in the optional **exclusive mode**, against 54 ms with the Python engine. Your numbers depend on the audio driver.
- **Self-healing audio**: unplugging a device, a sample-rate change, sleep and resume, or an audio service restart make the engine reconnect on its own; clock drift between the microphone and the virtual device is corrected sample by sample.
- **Latency control** (Python engine): buffer size of 64, 128, 256 or 512 samples, or `auto`.
- **Live status**: sample rate, estimated latency, CPU load, input and output meters, gate state and a clip indicator, with a spectrum, pulse or 3D cascade visualizer.
- **Easy and Advanced views**: Easy shows presets and two sliders; Advanced shows the full FX rack.
- **Tray icon**: start or stop the engine, open the dashboard or exit, from the notification area.

### Requirements

- Windows 10 or 11, macOS 11 or newer, or a Linux desktop with PipeWire or PulseAudio. The launcher is `run.bat` on Windows and `run.sh` on macOS and Linux.
- Python 3.10 or newer, on `PATH`.
- Node.js 20.19+ or 22.12+ (Node 21 is not supported), only to build the dashboard. If `backend\static\index.html` already exists, the build is skipped and Node is not needed.
- The HushCore virtual device, to pass the processed voice to other apps (see Installation). On Linux, `libportaudio2` for the Python engine and `libpulse-dev` to build the native one.
- Optional, for the native engine: [Rust](https://rustup.rs/) (stable). Without it HushCore uses the Python engine. Alternatively, download `hushcore-engine-*-windows-x64.zip` from the latest release and put `hushcore-engine.exe` in `backend\bin`.
- Optional, for HQ AI: PyTorch and DeepFilterNet, which is a large download. HQ AI runs only at 48 kHz; at other sample rates it uses Eco DSP.

### Installation

On Windows, double-click `run.bat` in the project folder or run it from a terminal. On macOS and Linux, run `./run.sh`. Both take the same options.

```bat
run.bat
```

It does the following:

1. Builds the dashboard into `backend\static`. This happens only when that folder is missing or you pass `--rebuild`.
2. Creates `venv` and installs `backend\requirements.txt`. Packages are installed again only when that file changes.
3. Builds the native engine with `cargo` if Rust is installed and no engine binary exists yet. Without Rust it continues with the Python engine.
4. If DeepFilterNet is not installed, asks once whether to install the HQ AI components.
5. Starts the backend and opens `http://127.0.0.1:8000`.

| Option | Effect |
| --- | --- |
| `--rebuild` | Rebuild the dashboard even when `backend\static` exists |
| `--ai` | Install the HQ AI components without asking |
| `--no-ai` | Never ask about the HQ AI components |
| `--help` | Show usage and exit |

Then install the virtual device once:

| Platform | Install | Uninstall |
| --- | --- | --- |
| Windows (as administrator) | `powershell -ExecutionPolicy Bypass -File scripts\windows\install-driver.ps1` | `scripts\windows\uninstall-driver.ps1` |
| macOS | `sudo scripts/macos/install-plugin.sh` | `sudo scripts/macos/uninstall-plugin.sh` |
| Linux | `scripts/linux/install-virtual-mic.sh` | `scripts/linux/uninstall-virtual-mic.sh` |

Windows loads only drivers signed by Microsoft; until a signed release is published, the driver needs test signing on a development machine (see [docs/SIGNING.md](docs/SIGNING.md)). On Linux the install step is optional: without it, the engine creates the virtual device when it starts and removes it when it quits. `hushcore-engine virtual-device` reports whether it is installed. Building from source on each platform is described in [docs/BUILDING.md](docs/BUILDING.md).

### Usage

1. Start HushCore with `run.bat` or `./run.sh`. The dashboard opens at `http://127.0.0.1:8000`.
2. Set **Microphone (input device)** to your physical microphone.
3. Leave **Broadcast output** on **Auto**, or pick **HushCore Sink**. The dashboard shows whether HushCore Microphone is installed.
4. Click **START ENGINE**.
5. In Discord, OBS or Zoom, set the input device to **HushCore Microphone**. Keep the output on your headphones or speakers.

```
Physical mic -> HushCore -> HushCore Sink
                                | (HushCore virtual device)
                       HushCore Microphone -> Discord / OBS / Zoom
```

> **Feedback:** if the broadcast output is your speakers or headphones, you hear your own processed voice, and the dashboard shows a warning. Keep the broadcast output on HushCore Sink, and leave **Hear myself (monitor)** off unless you want to monitor.

If a device disappears while the engine runs, the dashboard shows **RECONNECTING** and the engine reopens the devices as soon as they are back.

**Noise suppression.** ECO DSP is always available. HQ AI uses DeepFilterNet when it can be imported, and runs only at 48 kHz. Otherwise, or for any frame where DeepFilterNet fails, it falls back to ECO DSP. The **AI ENGINE** status shows whether DeepFilterNet loaded.

**Signal chain.** input gain, noise gate, noise suppression, de-esser, EQ, compressor, exciter, VST3 plugin, limiter, output gain.

**Presets.** Apply **Bypass**, **Clean voice**, **Gaming** or **Broadcaster** from the Easy view. To save the current DSP settings, type a name next to **SAVE**. Names are 1 to 32 letters, digits, spaces, `-` or `_`, and cannot reuse a built-in name. Saved presets hold DSP settings only; applying one leaves devices, gains, buffer size and the VST3 path unchanged.

**VST3.** In the **VST3 HOST** tab, enter the path to a `.vst3` file, click **LOAD**, enable the plugin and use **OPEN PLUGIN EDITOR**. If a plugin fails, HushCore bypasses it until its path changes or it is disabled.

**Live status.** The status strip shows the sample rate, an estimated latency (one 10 ms processing frame plus the audio stream latency) and CPU load as a share of that frame's time budget. **CLIP** lights when the output reaches 0.99 of full scale and stays lit for one second after the last clip.

**Tray icon.** Right-click the HushCore icon in the notification area to open the dashboard, start or stop the audio engine, or exit.

### Configuration

Set these environment variables before running `run.bat` or `run.sh`:

| Variable | Default | Meaning |
| --- | --- | --- |
| `HUSHCORE_PORT` | `8000` | Port for the dashboard and API. Invalid values fall back to 8000. |
| `HUSHCORE_NO_BROWSER` | unset | Set to `1` to keep HushCore from opening a browser window at start. |
| `HUSHCORE_ENGINE` | `auto` | `auto` uses the native engine when available, `native` or `python` forces one. Switchable in the dashboard under Advanced. |

Settings are stored in `backend\settings.json` and your presets in `backend\user_presets.json`. Both files are git-ignored. Delete either one to reset it.

**Exclusive mode** (the **Low-latency exclusive mode** switch, Windows only) gives the lowest latency, but other apps cannot use the selected devices while it is on. If the device does not support it, HushCore falls back to shared mode and says so. The buffer size setting applies to the Python engine only and takes effect when the engine starts.

Requests to `/api` must send the per-session key in an `X-API-Key` header. `/api/health` is public.

### Contributing

Run `run.bat` or `run.sh` once first; it creates `venv`. Then, in a terminal (Windows paths shown):

```bat
venv\Scripts\activate
pip install -r backend/requirements-dev.txt
python -m pytest backend/tests -q
cargo test --release --manifest-path engine/Cargo.toml
cd frontend
npm install
npm run dev
```

`npm run dev` serves the dashboard on `http://127.0.0.1:3000` with hot reload, for layout work only. It has no API proxy and the API key comes only from the backend, so its controls cannot reach the backend. For end-to-end testing, run `run.bat --rebuild` and use port 8000. CI builds and tests the engine on Windows, macOS and Linux, builds the Windows driver and the macOS plug-in, installs the plug-in and checks the loopback on a macOS runner, runs the Linux virtual microphone end to end in Docker, and runs the backend tests on Python 3.10 and 3.12. The engine's JSON-lines control protocol is in `engine/PROTOCOL.md`; architecture, building, signing and benchmarks are in [`docs/`](docs/ARCHITECTURE.md).

### License

MIT. © 2026 𝓐.𝓒.𝓑. See `LICENSE` for details.

[⬆ Back to top](#top)

---
<a id="turkce"></a>
## Türkçe

### Genel Bakış

HushCore mikrofonunuzdan gelen sesi gerçek zamanlı olarak işler ve sonucu kendi sanal mikrofonu olan **HushCore Microphone** üzerinden yayınlar. Discord, OBS, Zoom ve diğer uygulamalarda bu mikrofonu seçtiğinizde ham giriş yerine işlenmiş sesi alırsınız. Üçüncü taraf sanal ses yazılımına gerek yoktur: her platformda HushCore'un işletim sisteminin ses API'leriyle yazılmış kendi bileşeni çalışır (Windows'ta bir WaveRT sürücüsü, macOS'ta bir Core Audio eklentisi, Linux'ta PipeWire ya da PulseAudio sanal aygıtı).

Her şey kendi bilgisayarınızda çalışır. Ses kare kare CPU üzerinde işlenir ve hiçbir yere gönderilmez. Panel ve API yalnızca `127.0.0.1` üzerinde dinler; her başlatmada yeni bir API anahtarı üretilir ve panel bu anahtarı otomatik olarak alır.

### Özellikler

- **Kendi sanal mikrofonu**: Windows, macOS ve Linux'ta her platform için tek bir betikle kurulur ve kaldırılır. Bkz. [docs/VIRTUAL_DEVICE.md](docs/VIRTUAL_DEVICE.md).
- **Gürültü engelleme**, üç modda: `OFF`, `ECO DSP` (spektral çıkarma, yapay zeka bağımlılığı yok) ve `HQ AI` (DeepFilterNet, isteğe bağlı).
- **Studio FX rafı**: gürültü kapısı (gate), 3 bantlı EQ, kompresör, de-esser, harmonik vokal exciter ve tepe sınırlayıcı (limiter).
- **VST3 barındırma**: herhangi bir `.vst3` eklentisini [pedalboard](https://github.com/spotify/pedalboard) ile yükleyin ve eklentinin kendi editörünü panelden açın.
- **Hazır ayarlar**: dört yerleşik ön ayar ve kendi kaydettiğiniz ayarlar (`backend/user_presets.json`).
- **Yerel ses motoru**: derlendiğinde zinciri gerçek zamanlı bir Rust motoru (Windows'ta WASAPI, macOS'ta Core Audio, Linux'ta PulseAudio/PipeWire; yaklaşık %1 CPU) çalıştırır; VST3 kullanıldığında ya da yerel motor yoksa Python motoru devralır. Test bilgisayarımızda (Realtek, paylaşımlı mod 10 ms ile sınırlı) uçtan uca gecikme paylaşımlı modda yaklaşık 35 ms, isteğe bağlı **exclusive modda** 17 ms oldu; Python motorunda 54 ms. Sizin değerleriniz ses sürücüsüne bağlıdır.
- **Kendini toparlayan ses**: aygıtın çıkarılması, örnekleme hızının değişmesi, uyku ve uyanma ya da ses servisinin yeniden başlaması durumunda motor aygıtlara kendisi yeniden bağlanır. Mikrofon ile sanal aygıt arasındaki saat kayması örnek örnek düzeltilir.
- **Gecikme kontrolü** (Python motoru): 64, 128, 256 veya 512 örneklik tampon boyutu ya da `auto`.
- **Canlı durum**: örnekleme hızı, tahmini gecikme, CPU yükü, giriş ve çıkış ölçerleri, gate durumu, kırpma göstergesi ve spektrum, pulse ya da 3D cascade görselleştiricisi.
- **Easy ve Advanced görünümleri**: Easy'de hazır ayarlar ile iki kaydırıcı, Advanced'de tüm FX rafı bulunur.
- **Tepsi simgesi**: bildirim alanından motoru başlatıp durdurun, paneli açın ya da uygulamadan çıkın.

### Gereksinimler

- Windows 10 veya 11, macOS 11 veya daha yeni ya da PipeWire veya PulseAudio kullanan bir Linux masaüstü. Başlatıcı Windows'ta `run.bat`, macOS ve Linux'ta `run.sh` dosyasıdır.
- Python 3.10 veya daha yeni, `PATH` içinde.
- Node.js 20.19+ ya da 22.12+ (Node 21 desteklenmez); yalnızca paneli derlemek için gerekir. `backend\static\index.html` zaten varsa derleme atlanır ve Node gerekmez.
- İşlenmiş sesi diğer uygulamalara aktarmak için HushCore sanal aygıtı (bkz. Kurulum). Linux'ta Python motoru için `libportaudio2`, yerel motoru derlemek için `libpulse-dev`.
- İsteğe bağlı, yerel motor için: [Rust](https://rustup.rs/) (stable). Rust yoksa HushCore Python motorunu kullanır. Ya da son sürümden `hushcore-engine-*-windows-x64.zip` dosyasını indirip `hushcore-engine.exe` dosyasını `backend\bin` içine koyun.
- İsteğe bağlı HQ AI için PyTorch ve DeepFilterNet; büyük bir indirmedir. HQ AI yalnızca 48 kHz'de çalışır, diğer örnekleme hızlarında Eco DSP kullanılır.

### Kurulum

Windows'ta proje klasöründeki `run.bat` dosyasına çift tıklayın ya da terminalden çalıştırın. macOS ve Linux'ta `./run.sh` çalıştırın; seçenekler aynıdır.

```bat
run.bat
```

Bu dosya şunları yapar:

1. Paneli `backend\static` klasörüne derler. Bu yalnızca klasör yoksa ya da `--rebuild` verildiğinde yapılır.
2. `venv` sanal ortamını oluşturur ve `backend\requirements.txt` dosyasını kurar. Paketler yalnızca bu dosya değiştiğinde yeniden kurulur.
3. Rust kuruluysa ve henüz motor ikilisi yoksa yerel motoru `cargo` ile derler. Rust yoksa Python motoruyla devam eder.
4. DeepFilterNet kurulu değilse HQ AI bileşenlerini kurmak isteyip istemediğinizi bir kez sorar.
5. Backend'i başlatır ve `http://127.0.0.1:8000` adresini açar.

| Seçenek | Etkisi |
| --- | --- |
| `--rebuild` | `backend\static` olsa bile paneli yeniden derler |
| `--ai` | HQ AI bileşenlerini sormadan kurar |
| `--no-ai` | HQ AI bileşenleri hakkında hiç sormaz |
| `--help` | Kullanım bilgisini gösterip çıkar |

Ardından sanal aygıtı bir kez kurun:

| Platform | Kurulum | Kaldırma |
| --- | --- | --- |
| Windows (yönetici olarak) | `powershell -ExecutionPolicy Bypass -File scripts\windows\install-driver.ps1` | `scripts\windows\uninstall-driver.ps1` |
| macOS | `sudo scripts/macos/install-plugin.sh` | `sudo scripts/macos/uninstall-plugin.sh` |
| Linux | `scripts/linux/install-virtual-mic.sh` | `scripts/linux/uninstall-virtual-mic.sh` |

Windows yalnızca Microsoft imzalı sürücüleri yükler; imzalı bir sürüm yayımlanana kadar sürücü, geliştirme makinesinde test imzalama ister (bkz. [docs/SIGNING.md](docs/SIGNING.md)). Linux'ta kurulum isteğe bağlıdır: kurulmazsa motor sanal aygıtı başlarken oluşturur, kapanırken kaldırır. `hushcore-engine virtual-device` aygıtın kurulu olup olmadığını gösterir. Her platformda kaynaktan derleme [docs/BUILDING.md](docs/BUILDING.md) içinde anlatılır.

### Kullanım

1. `run.bat` ya da `./run.sh` ile HushCore'u başlatın. Panel `http://127.0.0.1:8000` adresinde açılır.
2. **Microphone (input device)** alanından fiziksel mikrofonunuzu seçin.
3. **Broadcast output** alanını **Auto** bırakın ya da **HushCore Sink** seçin. Panel, HushCore Microphone'un kurulu olup olmadığını gösterir.
4. **START ENGINE** düğmesine tıklayın.
5. Discord, OBS veya Zoom'da giriş aygıtını **HushCore Microphone** olarak ayarlayın. Çıkışı kulaklık ya da hoparlörünüzde bırakın.

```
Physical mic -> HushCore -> HushCore Sink
                                | (HushCore virtual device)
                       HushCore Microphone -> Discord / OBS / Zoom
```

> **Geri besleme:** Broadcast output olarak hoparlör ya da kulaklık seçerseniz kendi işlenmiş sesinizi duyarsınız ve panel bir uyarı gösterir. Broadcast output'u HushCore Sink olarak tutun; **Hear myself (monitor)** seçeneğini yalnızca kendinizi dinlemek istediğinizde açın.

Motor çalışırken bir aygıt kaybolursa panel **RECONNECTING** gösterir ve aygıt geri geldiğinde motor ona yeniden bağlanır.

**Gürültü engelleme.** ECO DSP her zaman kullanılabilir. HQ AI, DeepFilterNet yüklenebildiğinde çalışır ve yalnızca 48 kHz'de etkindir. Aksi halde, ya da DeepFilterNet'in bir karede hata verdiği durumlarda ECO DSP'ye döner. **AI ENGINE** durumu DeepFilterNet'in yüklenip yüklenmediğini gösterir.

**Sinyal zinciri.** Giriş kazancı, gate, gürültü engelleme, de-esser, EQ, kompresör, exciter, VST3 eklentisi, limiter, çıkış kazancı.

**Hazır ayarlar.** Easy görünümündeki **Bypass**, **Clean voice**, **Gaming** veya **Broadcaster** ayarlarından birini uygulayın. Mevcut DSP ayarlarını kaydetmek için **SAVE** yanındaki alana bir ad yazın. Adlar 1 ile 32 karakter arasında olmalı; harf, rakam, boşluk, `-` veya `_` içerebilir ve yerleşik bir adı kullanamaz. Kaydedilen ayarlar yalnızca DSP değerlerini tutar; bir ön ayar uygulandığında aygıtlar, kazançlar, tampon boyutu ve VST3 yolu değişmez.

**VST3.** **VST3 HOST** sekmesinde bir `.vst3` dosyasının yolunu girin, **LOAD** düğmesine tıklayın, eklentiyi etkinleştirin ve **OPEN PLUGIN EDITOR** ile eklentinin editörünü açın. Eklenti hata verirse, yolu değişene ya da eklenti devre dışı bırakılana kadar atlanır.

**Canlı durum.** Durum şeridi örnekleme hızını, tahmini gecikmeyi (bir 10 ms işleme karesi artı ses akışının gecikmesi) ve karenin süre bütçesine oranla CPU yükünü gösterir. Çıkış tam ölçeğin 0,99'una ulaştığında **CLIP** yanar ve son kırpmadan sonra bir saniye yanık kalır.

**Tepsi simgesi.** Bildirim alanındaki HushCore simgesine sağ tıklayarak paneli açabilir, ses motorunu başlatabilir ya da durdurabilir veya uygulamadan çıkabilirsiniz.

### Yapılandırma

`run.bat` ya da `run.sh` çalıştırılmadan önce şu ortam değişkenlerini ayarlayabilirsiniz:

| Değişken | Varsayılan | Anlamı |
| --- | --- | --- |
| `HUSHCORE_PORT` | `8000` | Panel ve API portu. Geçersiz değerler 8000'e döner. |
| `HUSHCORE_NO_BROWSER` | tanımsız | Başlangıçta tarayıcının açılmasını engellemek için `1` yapın. |
| `HUSHCORE_ENGINE` | `auto` | `auto` varsa yerel motoru kullanır; `native` ya da `python` birini zorlar. Panelde Advanced altından da değiştirilebilir. |

Ayarlar `backend\settings.json`, kendi ön ayarlarınız `backend\user_presets.json` dosyasında saklanır. İki dosya da Git tarafından yok sayılır; birini silmek onu sıfırlar.

**Exclusive mod** (**Low-latency exclusive mode** anahtarı, yalnızca Windows) en düşük gecikmeyi verir, ancak açıkken seçili aygıtları başka uygulamalar kullanamaz. Aygıt desteklemiyorsa HushCore paylaşımlı moda döner ve bunu bildirir. Tampon boyutu ayarı yalnızca Python motoru için geçerlidir ve motor başlatıldığında uygulanır.

`/api` altındaki istekler oturuma özel anahtarı `X-API-Key` başlığında göndermelidir. `/api/health` herkese açıktır.

### Katkı

Önce `run.bat` ya da `run.sh` bir kez çalıştırın; `venv` klasörünü o oluşturur. Ardından terminalde (Windows yolları gösterilmiştir):

```bat
venv\Scripts\activate
pip install -r backend/requirements-dev.txt
python -m pytest backend/tests -q
cargo test --release --manifest-path engine/Cargo.toml
cd frontend
npm install
npm run dev
```

`npm run dev` paneli `http://127.0.0.1:3000` adresinde canlı yenilemeyle açar, ancak yalnızca arayüz çalışması içindir. API proxy'si yoktur ve API anahtarını yalnızca backend sağlar; bu yüzden kontroller backend'e ulaşamaz. Uçtan uca test için `run.bat --rebuild` çalıştırıp 8000 portunu kullanın. CI, her push ve pull request'te motoru Windows, macOS ve Linux'ta derleyip test eder, Windows sürücüsünü ve macOS eklentisini derler, eklentiyi bir macOS makinesine kurup geri döngüyü sınar, Linux sanal mikrofonunu Docker içinde uçtan uca çalıştırır ve backend testlerini Python 3.10 ile 3.12'de koşturur. Motorun JSON-lines kontrol protokolü `engine/PROTOCOL.md` içinde; mimari, derleme, imzalama ve ölçümler [`docs/`](docs/ARCHITECTURE.md) klasöründedir.

### Lisans

MIT Lisansı ile dağıtılır. © 2026 𝓐.𝓒.𝓑. Ayrıntılar için `LICENSE` dosyasına bakın.

[⬆ Başa Dön](#top)
