# ⚡ HUSHCORE | Yerel Gerçek Zamanlı Mikrofon İşlemci

🌐 **[Click here for English Documentation](README.md)**

HushCore, mikrofondan gelen sesinizi gerçek zamanlı olarak iyileştiren, gizlilik odaklı ve düşük gecikmeli yerel bir ses işleme uygulamasıdır. Giriş sesini yakalar, dinamik DSP filtreleri (Gate, EQ, Compressor, Limiter) ve Gürültü Engelleme (Eco DSP veya HQ AI) uygulayarak çıkışı Discord, OBS veya Zoom gibi uygulamalarda kullanabilmeniz için sanal bir ses kablosuna yönlendirir.

---

## 🚀 Özellikler

*   **%100 Yerel ve Gizli**: Ses akışları buluta gönderilmez. Her şey yerel işlemciniz üzerinde kare kare işlenir.
*   **Çift Modlu Gürültü Engelleme**:
    *   **Eco DSP**: Dış yapay zeka bağımlılığı olmayan, hafif spektral çıkarma algoritması. Ultra düşük işlemci (CPU) harcar.
    *   **HQ AI**: CPU üzerinde **DeepFilterNet** derin öğrenme modelini çalıştırarak kristal netliğinde ses sağlar.
*   **Studio FX Rafı**:
    *   *Noise Gate (Gürültü Kapısı)*: Konuşmadığınızda arka plan seslerini ve mekanik klavye tıkırtılarını keser.
    *   *3-Bant Parametrik EQ*: Sesinizin sıcaklığını (Bass), netliğini (Mids) ve tiz netliğini (Treble) şekillendirir.
    *   *Kompresör*: Ses seviyesi dalgalanmalarını dengeleyerek çok kısık veya çok yüksek patlayan sesleri eşitler.
    *   *Limitleyici (Limiter)*: Dijital patlamaları ve ses bozulmalarını önler (>0 dBFS).
*   **Gecikmesiz Canlı Metreler**: forced reflow (tarayıcı düzen yükü) oluşturmayan, GPU hızlandırmalı CSS dönüşümleri sayesinde 60fps akıcı arayüz.
*   **Yönlendirme Uyarısı Paneli**: Çıkış doğrudan hoparlöre verildiğinde (kendi sesinizi duyup yankı yapmaması için) arayüzde anlık uyarılar gösterir.

---

## 🛠️ Gereksinimler

1.  **Node.js** (v18 veya üzeri) - [İndir](https://nodejs.org/)
2.  **Python** (3.8 - 3.12 önerilir) - [İndir](https://www.python.org/)
3.  **VB-Audio Virtual Cable** (Sesi diğer uygulamalara aktarmak için şarttır) - [İndir](https://vb-audio.com/Cable/)

---

## ⚙️ Kurulum ve Çalıştırma

### Windows (Hızlı Başlangıç)
Proje dizinindeki **`run.bat`** dosyasına çift tıklamanız yeterlidir. Script şunları yapar:
1.  Frontend modüllerini kurar ve Vite ile derler.
2.  Python Sanal Ortamı (`venv`) hazırlar ve gerekli kütüphaneleri yükler.
3.  DeepFilterNet AI kullanmak isteyip istemediğinizi sorar (İsteğe bağlı, ~1.5 GB PyTorch indirir).
4.  Yerel sunucuyu çalıştırır.

---

## 🎧 Ses Yönlendirme Kılavuzu

HushCore'u Discord, Zoom veya OBS gibi uygulamalarda mikrofon girdisi olarak kullanmak için yönlendirmeyi doğru yapmalısınız:

```
 [Fiziksel Mikrofon] ---> [HUSHCORE (Giriş)] 
                               |
                       (DSP & AI İşleme)
                               |
                               v
                     [HUSHCORE (Çıkış)] ---> [VB-Cable Input (Sanal Giriş)]
                                                         |
                                                  (Sanal Aktarım)
                                                         |
                                                         v
                                              [VB-Cable Output] ---> [Discord/OBS/Zoom Giriş]
```

### 1. HushCore Panel Ayarları
1.  Tarayıcınızda **`http://127.0.0.1:8000`** adresini açın.
2.  Giriş Aygıtı (Microphone) olarak kendi fiziksel mikrofonunuzu seçin.
3.  Çıkış Aygıtı (Virtual Cable) olarak **`CABLE Input (VB-Audio Virtual Cable)`** seçin.
4.  **`⚡ START ENGINE`** butonuna basarak ses motorunu çalıştırın.
5.  *İsteğe bağlı*: Filtrelenmiş sesinizi dinlemek isterseniz, **Hear Myself** seçeneğini aktif edip kulaklığınızı seçebilirsiniz.

> [!WARNING]
> **Neden doğrudan kendi sesimi duyuyorum?**
> Çıkış aygıtı olarak kulaklığınızı ya da hoparlörünüzü seçerseniz, sesiniz doğrudan size çalınacaktır. Bu yüzden çıkışı mutlaka **VB-Cable Input** olarak ayarlayın. Kendi sesinizi test etmek istemediğiniz sürece "Hear Myself" özelliğini kapalı tutun.

### 2. İstemci Uygulama Ayarları
Discord, OBS veya Zoom üzerinde:
1.  Ayarlar -> Ses ve Görüntü bölümüne gidin.
2.  **Giriş Aygıtını (Input Device)** **`CABLE Output (VB-Audio Virtual Cable)`** olarak seçin.
3.  **Çıkış Aygıtını (Output Device)** kendi hoparlör/kulaklığınız olarak bırakın.

---

## 💻 Teknoloji Yığını

*   **Arka Plan (Backend)**: Python, FastAPI, WebSockets (Gerçek zamanlı seviye ve FFT akışı), Uvicorn.
*   **Ses İşleme (Audio DSP)**: NumPy, SciPy Signal, SoundDevice (PortAudio sarmalayıcı).
*   **AI Motoru**: PyTorch, DeepFilterNet (Derin öğrenme ile ses iyileştirme).
*   **Arayüz (Frontend)**: Vanilla HTML5/CSS3 (Glassmorphic neon tema), Javascript (ES modülleri), Vite derleyici.

---

## 📄 Lisans

MIT Lisansı ile dağıtılmaktadır. Detaylar için `LICENSE` dosyasına bakabilirsiniz.
