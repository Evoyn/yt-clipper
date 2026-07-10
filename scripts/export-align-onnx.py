"""Export cahya/wav2vec2-large-xlsr-indonesian (character-CTC) to ONNX for the
forced-alignment caption timing pass (ADR 0053/0054, models/w2v2-align-id).

Run via uv (no permanent env; operator-initiated network, Offline constraint):
  uv run --with torch --with transformers --with onnx \
    python scripts/export-align-onnx.py models/w2v2-align-id

fp32, opset 17, dynamic batch/samples axes. Input contract of the Rust aligner
(yc-transcribe align): RAW 16 kHz mono f32 samples, NO zero-mean/unit-var
normalization -- measured 2026-07-11 (ADR 0054): normalization trades the
spike's one miss (GUE) for a new one (PINGUIN duplicate-grab), and the operator
approved the RAW burn. Do not "fix" this to match preprocessor_config.json.
"""
import os
import sys

import torch
from transformers import Wav2Vec2ForCTC, Wav2Vec2Processor

MODEL = "cahya/wav2vec2-large-xlsr-indonesian"

out_dir = sys.argv[1] if len(sys.argv) > 1 else os.path.join("models", "w2v2-align-id")
os.makedirs(out_dir, exist_ok=True)
onnx_path = os.path.join(out_dir, "model.onnx")

proc = Wav2Vec2Processor.from_pretrained(MODEL)
model = Wav2Vec2ForCTC.from_pretrained(MODEL).eval()
print("loaded", MODEL)

if os.path.exists(onnx_path):
    print(onnx_path, "already present -- skipping export. Delete it to re-export.")
else:
    dummy = torch.randn(1, 64000, dtype=torch.float32)
    torch.onnx.export(
        model,
        (dummy,),
        onnx_path,
        input_names=["input_values"],
        output_names=["logits"],
        dynamic_axes={
            "input_values": {0: "batch", 1: "samples"},
            "logits": {0: "batch", 1: "frames"},
        },
        opset_version=17,
        do_constant_folding=True,
        dynamo=False,
    )
    print("exported", onnx_path)

from huggingface_hub import hf_hub_download
import shutil

vocab_src = hf_hub_download(MODEL, "vocab.json")
shutil.copyfile(vocab_src, os.path.join(out_dir, "vocab.json"))
print("copied vocab.json; done")
