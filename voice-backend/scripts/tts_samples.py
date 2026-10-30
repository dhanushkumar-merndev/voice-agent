#!/usr/bin/env python3
"""Makes phone-quality (8 kHz) samples of candidate TTS voices, so a voice
can be picked by ear for each language.

Every voice in a language says the same call line (with a time and a
question, like a real reply). Output, per language:
  test/output/tts/<lang>/<NN>_<speaker>_<model>.wav   one clip per voice
  test/output/tts/<lang>/ALL_<lang>.wav               every clip in a row
  test/output/tts/index.csv                           voice, gender, model, ₹/min

Run from voice-backend/ with the Sarvam key loaded:
  set -a; . ../.env; set +a; python3 scripts/tts_samples.py
Only the Python standard library is used.
"""

import base64
import csv
import io
import json
import os
import sys
import urllib.error
import urllib.request
import wave

OUT = os.path.join("test", "output", "tts")
URL = "https://api.sarvam.ai/text-to-speech"
V4, V3 = "bulbul:v4-flash", "bulbul:v3"
# ₹ per 1k characters (config/pricing.toml) and characters spoken per call minute
# (MAX_TTS_CHARS_PER_CALL-style estimate: ~600 agent characters a minute).
RATE = {V4: 1.0, V3: 3.0}
CHARS_PER_MIN = 600

LINES = {
    "en-IN": "Hi, calling from Star Smile Dental. Tomorrow at 11:30 AM is free. Shall I book it for you?",
    "hi-IN": "नमस्ते, Star Smile Dental से कॉल है। कल सुबह 11:30 का slot free है। क्या मैं book कर दूँ?",
    "ta-IN": "வணக்கம், Star Smile Dental-லிருந்து அழைக்கிறோம். நாளை காலை 11:30 மணிக்கு நேரம் இருக்கிறது. புக் செய்யட்டுமா?",
    "te-IN": "నమస్తే, Star Smile Dental నుండి కాల్ చేస్తున్నాం. రేపు ఉదయం 11:30 కి సమయం ఖాళీగా ఉంది. బుక్ చేయమంటారా?",
    "kn-IN": "ನಮಸ್ಕಾರ, Star Smile Dental ಇಂದ ಕಾಲ್ ಮಾಡ್ತಿದ್ದೀವಿ. ನಾಳೆ ಬೆಳಿಗ್ಗೆ 11:30 ಕ್ಕೆ ಸಮಯ ಖಾಲಿ ಇದೆ. ಬುಕ್ ಮಾಡ್ಲಾ?",
    "ml-IN": "നമസ്കാരം, Star Smile Dental-ൽ നിന്നാണ് വിളിക്കുന്നത്. നാളെ രാവിലെ 11:30-ന് സമയം ഒഴിവുണ്ട്. ബുക്ക് ചെയ്യട്ടെ?",
    "bn-IN": "নমস্কার, Star Smile Dental থেকে ফোন করছি। কাল সকাল 11:30-এ সময় খালি আছে। বুক করে দেব?",
}

# (folder, language code, line key, [(speaker, model, gender)]). Current
# production voices are listed first in each language.
F, M = "female", "male"
VOICES = [
    ("en", "en-IN", "en-IN", [
        ("simran_en_conversation", V4, F), ("simran_en_customer", V4, F), ("simran_en_sales", V4, F),
        ("ishita_en_customer", V4, F), ("neha_en_customer", V4, F), ("shalini_en_customer", V4, F),
        ("roopa_en_conversational", V4, F), ("zarina_en_conversation", V4, F),
        ("sanchita_en_insurance", V4, F), ("dev_en_conversational", V4, M),
        ("deven_en_conversation", V4, M), ("shubh_en_narration", V4, M),
        ("simran", V3, F), ("ishita", V3, F), ("priya", V3, F),
    ]),
    ("hinglish", "hi-IN", "hi-IN", [
        ("simran_enhi_customer", V4, F), ("ishita_enhi_customer", V4, F),
        ("shalini_enhi_customer", V4, F), ("sunny_enhi_customer", V4, M),
    ]),
    ("hi", "hi-IN", "hi-IN", [
        ("ritu_hi_conversation", V4, F), ("ritu_hi_customer_warm", V4, F), ("ritu_hi_customer", V4, F),
        ("aparna_hi_customer", V4, F), ("roopa_hi_conversational", V4, F),
        ("kavya_hi_conversational", V4, F), ("aditya_hi_conversational", V4, M),
        ("mohit_hi_conversational", V4, M), ("ratan_hi_customer_expressive", V4, M),
        ("ritu", V3, F), ("priya", V3, F),
    ]),
    ("ta", "ta-IN", "ta-IN", [
        ("vijay_ta_narration", V4, M), ("gokul_ta_narration", V4, M),
        ("aravind_ta_ads", V4, M), ("aravind_ta_suspense", V4, M),
        ("kavitha", V3, F), ("priya", V3, F), ("vijay", V3, M),
    ]),
    ("te", "te-IN", "te-IN", [
        ("kavitha_te_conversation", V4, F), ("pooja_te_conversation", V4, F),
        ("kavitha_te_narration", V4, F), ("tarun_te_narration", V4, M),
        ("kavitha", V3, F), ("priya", V3, F),
    ]),
    ("kn", "kn-IN", "kn-IN", [
        ("chaitra_kn_conversation", V4, F), ("chaitra_kn_narration", V4, F),
        ("suchitra_kn_narration", V4, F), ("chetan_kn_conversation", V4, M),
        ("priya", V3, F), ("kavya", V3, F),
    ]),
    ("ml", "ml-IN", "ml-IN", [
        ("pooja", V3, F), ("priya", V3, F), ("kavitha", V3, F), ("shruti", V3, F),
        ("ritu", V3, F), ("vijay", V3, M),
    ]),
    ("bn", "bn-IN", "bn-IN", [
        ("roopa_bn_conversational", V4, F), ("arnab_bn_conversation", V4, M),
        ("bimal_bn_suspense", V4, M), ("roopa", V3, F), ("priya", V3, F),
    ]),
]


def synth(key, text, lang, speaker, model):
    body = json.dumps({
        "text": text, "target_language_code": lang, "speaker": speaker, "model": model,
        "speech_sample_rate": 8000, "enable_preprocessing": True,
    }).encode()
    req = urllib.request.Request(URL, data=body, method="POST", headers={
        "api-subscription-key": key, "Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=60) as r:
        return base64.b64decode(json.load(r)["audios"][0])


def frames(wav_bytes):
    with wave.open(io.BytesIO(wav_bytes)) as w:
        return w.getparams(), w.readframes(w.getnframes())


def main():
    key = os.environ.get("SARVAM_API_KEY")
    if not key:
        sys.exit("SARVAM_API_KEY is not set (run: set -a; . ../.env; set +a)")
    os.makedirs(OUT, exist_ok=True)
    rows, cost, failed = [], 0.0, []
    for folder, lang, line, voices in VOICES:
        text = LINES[line]
        d = os.path.join(OUT, folder)
        os.makedirs(d, exist_ok=True)
        joined, params = [], None
        for i, (speaker, model, gender) in enumerate(voices, 1):
            name = f"{i:02d}_{speaker}_{model.split(':')[1]}.wav"
            try:
                audio = synth(key, text, lang, speaker, model)
            except urllib.error.HTTPError as e:
                failed.append(f"{folder}/{speaker} ({model}): {e.code} {e.read()[:150]!r}")
                continue
            open(os.path.join(d, name), "wb").write(audio)
            cost += len(text) * RATE[model] / 1000
            p, pcm = frames(audio)
            params = params or p
            # 0.7 s of silence between voices in the combined file.
            joined.append(pcm + b"\0" * int(p.framerate * 0.7) * p.sampwidth * p.nchannels)
            rows.append({
                "language": folder, "file": f"{folder}/{name}", "speaker": speaker, "model": model,
                "gender": gender, "tts_inr_per_min": round(CHARS_PER_MIN * RATE[model] / 1000, 2),
                "seconds": round(len(pcm) / (p.framerate * p.sampwidth * p.nchannels), 1),
            })
            print(f"{folder:9} {name}")
        if params and joined:
            with wave.open(os.path.join(d, f"ALL_{folder}.wav"), "wb") as w:
                w.setparams(params)
                w.writeframes(b"".join(joined))
    with open(os.path.join(OUT, "index.csv"), "w", newline="") as f:
        w = csv.DictWriter(f, fieldnames=list(rows[0]))
        w.writeheader()
        w.writerows(rows)
    print(f"\n{len(rows)} clips in {OUT}/, cost about ₹{cost:.2f} + GST")
    for f in failed:
        print("FAILED", f)


if __name__ == "__main__":
    main()
