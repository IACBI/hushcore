<a id="top"></a>
# HushCore

Local, privacy-first real-time microphone processor for Windows. Noise suppression, a studio FX rack and routing to a virtual cable, all on your own CPU.

![License: MIT](https://img.shields.io/badge/license-MIT-green)
![Python 3.10+](https://img.shields.io/badge/python-3.10%2B-blue)
![Platform: Windows](https://img.shields.io/badge/platform-Windows-lightgrey)

**Read this in:** [English](#english) · [Türkçe](#turkce)

---
<a id="english"></a>
## English

### Overview

HushCore takes your microphone, cleans it up in real time and sends the result to a virtual audio cable. Discord, OBS, Zoom and other apps then receive the processed voice instead of the raw input.

Everything runs on your machine. Audio is processed frame by frame on the CPU and is never sent anywhere. The dashboard and API listen on `127.0.0.1` only, and each start generates a new API key that the dashboard receives automatically.

### Features

- **Noise suppression** in three modes: `OFF`, `ECO DSP` (spectral subtraction, no AI dependencies) and `HQ AI` (DeepFilterNet, optional).
- **Studio FX rack**: noise gate, 3-band EQ, compressor, de-esser, harmonic vocal exciter and peak limiter.
- **VST3 host**: load any `.vst3` plugin through [pedalboard](https://github.com/spotify/pedalboard) and open its own editor from the dashboard.
- **Presets**: four built-in presets, plus your own saved to `backend/user_presets.json`.
- **Native audio engine**: a real-time Rust engine (WASAPI, about 1% CPU) runs the chain when it is built, and the Python engine takes over for VST3 or when the native one is missing. On our test PC (Realtek, shared mode limited to 10 ms) end-to-end latency was about 35 ms shared and 17 ms in the optional **exclusive mode**, against 54 ms with the Python engine. Your numbers depend on the audio driver.
- **Latency control** (Python engine): buffer size of 64, 128, 256 or 512 samples, or `auto`.
- **Live status**: sample rate, estimated latency, CPU load, input and output meters, gate state and a clip indicator, with a spectrum, pulse or 3D cascade visualizer.
- **Easy and Advanced views**: Easy shows presets and two sliders; Advanced shows the full FX rack.
- **Tray icon**: start or stop the engine, open the dashboard or exit, from the notification area.

### Requirements

- Windows 10 or 11. The launcher is `run.bat`.
- Python 3.10 or newer, on `PATH`.
- Node.js 20.19 or newer, only to build the dashboard. If `backend\static\index.html` already exists, the build is skipped and Node is not needed.
- [VB-Audio Virtual Cable](https://vb-audio.com/Cable/) to pass the processed audio to other apps.
- Optional, for the native engine: [Rust](https://rustup.rs/) (stable). Without it HushCore uses the Python engine.
- Optional, for HQ AI: PyTorch and DeepFilterNet, which is a large download. HQ AI runs only at 48 kHz; at other sample rates it uses Eco DSP.

### Installation

Double-click `run.bat` in the project folder, or run it from a terminal:

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

### Usage

1. Start HushCore with `run.bat`. The dashboard opens at `http://127.0.0.1:8000`.
2. Set **Microphone (input device)** to your physical microphone.
3. Set **Broadcast output (virtual cable)** to **`CABLE Input (VB-Audio Virtual Cable)`**.
4. Click **START ENGINE**.
5. In Discord, OBS or Zoom, set the input device to **`CABLE Output (VB-Audio Virtual Cable)`**. Keep the output on your headphones or speakers.

```
Physical mic -> HushCore -> CABLE Input
                               | (virtual cable)
                            CABLE Output -> Discord / OBS / Zoom
```

> **Feedback:** if the broadcast output is your speakers or headphones, you hear your own processed voice, and the dashboard shows a warning. Keep the broadcast output on `CABLE Input`, and leave **Hear myself (monitor)** off unless you want to monitor.

**Noise suppression.** ECO DSP is always available. HQ AI uses DeepFilterNet when it can be imported, and runs only at 48 kHz. Otherwise, or for any frame where DeepFilterNet fails, it falls back to ECO DSP. The **AI ENGINE** status shows whether DeepFilterNet loaded.

**Signal chain.** input gain, noise gate, noise suppression, de-esser, EQ, compressor, exciter, VST3 plugin, limiter, output gain.

**Presets.** Apply **Bypass**, **Clean voice**, **Gaming** or **Broadcaster** from the Easy view. To save the current DSP settings, type a name next to **SAVE**. Names are 1 to 32 letters, digits, spaces, `-` or `_`, and cannot reuse a built-in name. Saved presets hold DSP settings only; applying one leaves devices, gains, buffer size and the VST3 path unchanged.

**VST3.** In the **VST3 HOST** tab, enter the path to a `.vst3` file, click **LOAD**, enable the plugin and use **OPEN PLUGIN EDITOR**. If a plugin fails, HushCore bypasses it until its path changes or it is disabled.

**Live status.** The status strip shows the sample rate, an estimated latency (one 10 ms processing frame plus the audio stream latency) and CPU load as a share of that frame's time budget. **CLIP** lights when the output reaches 0.99 of full scale and stays lit for one second after the last clip.

**Tray icon.** Right-click the HushCore icon in the notification area to open the dashboard, start or stop the audio engine, or exit.

### Configuration

Set these environment variables before running `run.bat`:

| Variable | Default | Meaning |
| --- | --- | --- |
| `HUSHCORE_PORT` | `8000` | Port for the dashboard and API. Invalid values fall back to 8000. |
| `HUSHCORE_NO_BROWSER` | unset | Set to `1` to keep HushCore from opening a browser window at start. |
| `HUSHCORE_ENGINE` | `auto` | `auto` uses the native engine when available, `native` or `python` forces one. Switchable in the dashboard under Advanced. |

Settings are stored in `backend\settings.json` and your presets in `backend\user_presets.json`. Both files are git-ignored. Delete either one to reset it.

**Exclusive mode** (the **Low-latency exclusive mode** switch) gives the lowest latency, but other apps cannot use the selected devices while it is on. If the device does not support it, HushCore falls back to shared mode and says so. The buffer size setting applies to the Python engine only and takes effect when the engine starts.

Requests to `/api` must send the per-session key in an `X-API-Key` header. `/api/health` is public.

### Contributing

Run `run.bat` once first; it creates `venv`. Then, in a terminal:

```bat
venv\Scripts\activate
pip install -r backend/requirements-dev.txt
python -m pytest backend/tests -q
cargo test --release --manifest-path engine/Cargo.toml
cd frontend
npm install
npm run dev
```

`npm run dev` serves the dashboard on `http://127.0.0.1:3000` with hot reload, for layout work only. It has no API proxy and the API key comes only from the backend, so its controls cannot reach the backend. For end-to-end testing, run `run.bat --rebuild` and use port 8000. CI runs the backend tests on Python 3.10 and 3.12, builds the dashboard and builds and tests the engine on every push and pull request. The engine's JSON-lines control protocol is documented in `engine/PROTOCOL.md`.

### License

MIT. © 2026 𝓐.𝓒.𝓑. See `LICENSE` for details.

[⬆ Back to top](#top)

---
<a id="turkce"></a>
## Türkçe

### Genel Bakış

HushCore mikrofonunuzdan gelen sesi gerçek zamanlı olarak işler ve sonucu bir sanal ses kablosuna gönderir. Discord, OBS, Zoom ve diğer uygulamalar ham girişi değil, işlenmiş sesi alır.

Her şey kendi bilgisayarınızda çalışır. Ses kare kare CPU üzerinde işlenir ve hiçbir yere gönderilmez. Panel ve API yalnızca `127.0.0.1` üzerinde dinler; her başlatmada yeni bir API anahtarı üretilir ve panel bu anahtarı otomatik olarak alır.

### Özellikler

- **Gürültü engelleme**, üç modda: `OFF`, `ECO DSP` (spektral çıkarma, yapay zeka bağımlılığı yok) ve `HQ AI` (DeepFilterNet, isteğe bağlı).
- **Studio FX rafı**: gürültü kapısı (gate), 3 bantlı EQ, kompresör, de-esser, harmonik vokal exciter ve tepe sınırlayıcı (limiter).
- **VST3 barındırma**: herhangi bir `.vst3` eklentisini [pedalboard](https://github.com/spotify/pedalboard) ile yükleyin ve eklentinin kendi editörünü panelden açın.
- **Hazır ayarlar**: dört yerleşik ön ayar ve kendi kaydettiğiniz ayarlar (`backend/user_presets.json`).
- **Yerel ses motoru**: derlendiğinde zinciri gerçek zamanlı bir Rust motoru (WASAPI, yaklaşık %1 CPU) çalıştırır; VST3 kullanıldığında ya da yerel motor yoksa Python motoru devralır. Test bilgisayarımızda (Realtek, paylaşımlı mod 10 ms ile sınırlı) uçtan uca gecikme paylaşımlı modda yaklaşık 35 ms, isteğe bağlı **exclusive modda** 17 ms oldu; Python motorunda 54 ms. Sizin değerleriniz ses sürücüsüne bağlıdır.
- **Gecikme kontrolü** (Python motoru): 64, 128, 256 veya 512 örneklik tampon boyutu ya da `auto`.
- **Canlı durum**: örnekleme hızı, tahmini gecikme, CPU yükü, giriş ve çıkış ölçerleri, gate durumu, kırpma göstergesi ve spektrum, pulse ya da 3D cascade görselleştiricisi.
- **Easy ve Advanced görünümleri**: Easy'de hazır ayarlar ile iki kaydırıcı, Advanced'de tüm FX rafı bulunur.
- **Tepsi simgesi**: bildirim alanından motoru başlatıp durdurun, paneli açın ya da uygulamadan çıkın.

### Gereksinimler

- Windows 10 veya 11. Başlatıcı `run.bat` dosyasıdır.
- Python 3.10 veya daha yeni, `PATH` içinde.
- Node.js 20.19 veya daha yeni; yalnızca paneli derlemek için gerekir. `backend\static\index.html` zaten varsa derleme atlanır ve Node gerekmez.
- İşlenmiş sesi diğer uygulamalara aktarmak için [VB-Audio Virtual Cable](https://vb-audio.com/Cable/).
- İsteğe bağlı, yerel motor için: [Rust](https://rustup.rs/) (stable). Rust yoksa HushCore Python motorunu kullanır.
- İsteğe bağlı HQ AI için PyTorch ve DeepFilterNet; büyük bir indirmedir. HQ AI yalnızca 48 kHz'de çalışır, diğer örnekleme hızlarında Eco DSP kullanılır.

### Kurulum

Proje klasöründeki `run.bat` dosyasına çift tıklayın ya da terminalden çalıştırın:

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

### Kullanım

1. `run.bat` ile HushCore'u başlatın. Panel `http://127.0.0.1:8000` adresinde açılır.
2. **Microphone (input device)** alanından fiziksel mikrofonunuzu seçin.
3. **Broadcast output (virtual cable)** alanından **`CABLE Input (VB-Audio Virtual Cable)`** seçeneğini seçin.
4. **START ENGINE** düğmesine tıklayın.
5. Discord, OBS veya Zoom'da giriş aygıtını **`CABLE Output (VB-Audio Virtual Cable)`** olarak ayarlayın. Çıkışı kulaklık ya da hoparlörünüzde bırakın.

```
Physical mic -> HushCore -> CABLE Input
                               | (virtual cable)
                            CABLE Output -> Discord / OBS / Zoom
```

> **Geri besleme:** Broadcast output olarak hoparlör ya da kulaklık seçerseniz kendi işlenmiş sesinizi duyarsınız ve panel bir uyarı gösterir. Broadcast output'u `CABLE Input` olarak tutun; **Hear myself (monitor)** seçeneğini yalnızca kendinizi dinlemek istediğinizde açın.

**Gürültü engelleme.** ECO DSP her zaman kullanılabilir. HQ AI, DeepFilterNet yüklenebildiğinde çalışır ve yalnızca 48 kHz'de etkindir. Aksi halde, ya da DeepFilterNet'in bir karede hata verdiği durumlarda ECO DSP'ye döner. **AI ENGINE** durumu DeepFilterNet'in yüklenip yüklenmediğini gösterir.

**Sinyal zinciri.** Giriş kazancı, gate, gürültü engelleme, de-esser, EQ, kompresör, exciter, VST3 eklentisi, limiter, çıkış kazancı.

**Hazır ayarlar.** Easy görünümündeki **Bypass**, **Clean voice**, **Gaming** veya **Broadcaster** ayarlarından birini uygulayın. Mevcut DSP ayarlarını kaydetmek için **SAVE** yanındaki alana bir ad yazın. Adlar 1 ile 32 karakter arasında olmalı; harf, rakam, boşluk, `-` veya `_` içerebilir ve yerleşik bir adı kullanamaz. Kaydedilen ayarlar yalnızca DSP değerlerini tutar; bir ön ayar uygulandığında aygıtlar, kazançlar, tampon boyutu ve VST3 yolu değişmez.

**VST3.** **VST3 HOST** sekmesinde bir `.vst3` dosyasının yolunu girin, **LOAD** düğmesine tıklayın, eklentiyi etkinleştirin ve **OPEN PLUGIN EDITOR** ile eklentinin editörünü açın. Eklenti hata verirse, yolu değişene ya da eklenti devre dışı bırakılana kadar atlanır.

**Canlı durum.** Durum şeridi örnekleme hızını, tahmini gecikmeyi (bir 10 ms işleme karesi artı ses akışının gecikmesi) ve karenin süre bütçesine oranla CPU yükünü gösterir. Çıkış tam ölçeğin 0,99'una ulaştığında **CLIP** yanar ve son kırpmadan sonra bir saniye yanık kalır.

**Tepsi simgesi.** Bildirim alanındaki HushCore simgesine sağ tıklayarak paneli açabilir, ses motorunu başlatabilir ya da durdurabilir veya uygulamadan çıkabilirsiniz.

### Yapılandırma

`run.bat` çalıştırılmadan önce şu ortam değişkenlerini ayarlayabilirsiniz:

| Değişken | Varsayılan | Anlamı |
| --- | --- | --- |
| `HUSHCORE_PORT` | `8000` | Panel ve API portu. Geçersiz değerler 8000'e döner. |
| `HUSHCORE_NO_BROWSER` | tanımsız | Başlangıçta tarayıcının açılmasını engellemek için `1` yapın. |
| `HUSHCORE_ENGINE` | `auto` | `auto` varsa yerel motoru kullanır; `native` ya da `python` birini zorlar. Panelde Advanced altından da değiştirilebilir. |

Ayarlar `backend\settings.json`, kendi ön ayarlarınız `backend\user_presets.json` dosyasında saklanır. İki dosya da Git tarafından yok sayılır; birini silmek onu sıfırlar.

**Exclusive mod** (**Low-latency exclusive mode** anahtarı) en düşük gecikmeyi verir, ancak açıkken seçili aygıtları başka uygulamalar kullanamaz. Aygıt desteklemiyorsa HushCore paylaşımlı moda döner ve bunu bildirir. Tampon boyutu ayarı yalnızca Python motoru için geçerlidir ve motor başlatıldığında uygulanır.

`/api` altındaki istekler oturuma özel anahtarı `X-API-Key` başlığında göndermelidir. `/api/health` herkese açıktır.

### Katkı

Önce `run.bat` bir kez çalıştırın; `venv` klasörünü o oluşturur. Ardından terminalde:

```bat
venv\Scripts\activate
pip install -r backend/requirements-dev.txt
python -m pytest backend/tests -q
cargo test --release --manifest-path engine/Cargo.toml
cd frontend
npm install
npm run dev
```

`npm run dev` paneli `http://127.0.0.1:3000` adresinde canlı yenilemeyle açar, ancak yalnızca arayüz çalışması içindir. API proxy'si yoktur ve API anahtarını yalnızca backend sağlar; bu yüzden kontroller backend'e ulaşamaz. Uçtan uca test için `run.bat --rebuild` çalıştırıp 8000 portunu kullanın. CI, her push ve pull request'te backend testlerini Python 3.10 ve 3.12 ile çalıştırır, paneli derler, motoru derleyip test eder. Motorun JSON-lines kontrol protokolü `engine/PROTOCOL.md` içinde belgelidir.

### Lisans

MIT Lisansı ile dağıtılır. © 2026 𝓐.𝓒.𝓑. Ayrıntılar için `LICENSE` dosyasına bakın.

[⬆ Başa Dön](#top)
