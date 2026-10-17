// memhub build script
//
// Stages the bundled retrieval model files into OUT_DIR so the main crate
// can include them via include_bytes!. Two models are bundled:
//
//   * nomic-embed-text-v1.5 — bi-encoder used by the hybrid recall path
//     and the code index. ~138 MB (int8 dynamic-quantized
//     `onnx/model_int8.onnx`, 768-dim, mean pooling, Apache-2.0). Source:
//     nomic-ai/nomic-embed-text-v1.5, pinned to commit
//     e9b6763023c676ca8431644204f50c2b100d9aab (not resolve/main).
//
//   * ms-marco-MiniLM-L-6-v2 — cross-encoder re-ranker bundled by the
//     task-#21 work. ~22 MB (int8-quantized `model_int8.onnx`; issue #75
//     / Q18, decision 148 — was ~91 MB fp32). Source:
//     Xenova/ms-marco-MiniLM-L-6-v2@main (an ONNX export of
//     cross-encoder/ms-marco-MiniLM-L-6-v2). Selected over BGE-reranker-
//     v2-m3 in the bake-off (decisions 68–70): +17.7pp Recall@1 over
//     baseline, 15× faster than BGE-v2-m3, no keyword regressions.
//
// On a clean build this downloads ~160 MB total from Hugging Face. On
// rebuilds the staged files are reused if their SHA256 matches the
// pinned value. All hashes were computed locally over the downloaded
// bytes (or, for the ONNX models, equal the LFS object id Hugging Face
// publishes for the pinned file).
//
// BUILD-TIME NETWORK CONTRACT (task 52). memhub's offline-first
// principle is a *runtime* guarantee: the built binary never reaches the
// network (the lone exception is the explicit, opt-in `memhub metrics
// calibrate`). Building from source is a different matter — this fetch is
// the one point where producing the binary touches the network. The
// OUT_DIR staging cache is the only offline fallback, and `cargo clean`
// discards it, so the next build re-fetches. Airgapped *source* builds
// are therefore unsupported; there is no offline asset override today.

use std::env;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

struct ModelFile {
    /// Path inside the Hugging Face repo, relative to the model's base URL.
    remote_path: &'static str,
    /// Filename to use under OUT_DIR/<model_dir>/.
    local_name: &'static str,
    /// Hex-encoded SHA256 of the expected file contents.
    sha256: &'static str,
}

struct ModelBundle {
    /// Subdirectory under OUT_DIR where the files are staged.
    local_dir: &'static str,
    /// Hugging Face base URL (resolve/<ref>).
    base_url: &'static str,
    files: &'static [ModelFile],
}

const NOMIC_EMBED: ModelBundle = ModelBundle {
    local_dir: "nomic-embed-text-v1.5",
    base_url: "https://huggingface.co/nomic-ai/nomic-embed-text-v1.5/resolve/e9b6763023c676ca8431644204f50c2b100d9aab",
    files: &[
        // int8 dynamic-quantized export. The SHA-256 is the HF LFS oid for
        // the pinned file, read from the repo file tree at the pinned commit
        // and re-verified over the downloaded bytes by ensure_file below.
        // Staged as `model.onnx` so embeddings.rs names it generically.
        ModelFile {
            remote_path: "onnx/model_int8.onnx",
            local_name: "model.onnx",
            sha256: "b4342336debaea79de872370664b0aaeb67dea4605513d00ee236ea871a81f27",
        },
        ModelFile {
            remote_path: "tokenizer.json",
            local_name: "tokenizer.json",
            sha256: "d241a60d5e8f04cc1b2b3e9ef7a4921b27bf526d9f6050ab90f9267a1f9e5c66",
        },
        ModelFile {
            remote_path: "config.json",
            local_name: "config.json",
            sha256: "9ab00bd92cee80a569f708140b7b6c1661a65891ff3765b1519e181ba2f2c92b",
        },
        ModelFile {
            remote_path: "special_tokens_map.json",
            local_name: "special_tokens_map.json",
            sha256: "5d5b662e421ea9fac075174bb0688ee0d9431699900b90662acd44b2a350503a",
        },
        ModelFile {
            remote_path: "tokenizer_config.json",
            local_name: "tokenizer_config.json",
            sha256: "d7e0000bcc80134debd2222220427e6bf5fa20a669f40a0d0d1409cc18e0a9bc",
        },
    ],
};

const RERANKER: ModelBundle = ModelBundle {
    local_dir: "ms-marco-MiniLM-L-6-v2",
    base_url: "https://huggingface.co/Xenova/ms-marco-MiniLM-L-6-v2/resolve/main",
    files: &[
        // int8-quantized cross-encoder (issue #75 / Q18, decision 148).
        // Xenova publishes several quant variants under onnx/;
        // `model_int8.onnx` is the QInt8 dynamic quantization (91 MB fp32
        // -> 22 MB). Staged under the same local name `model.onnx` so
        // rerank.rs's include_bytes! is unchanged. The SHA-256 is the HF
        // LFS oid (x-linked-etag) for the pinned file, read from the repo
        // file tree and re-verified over the downloaded bytes by
        // ensure_file below.
        ModelFile {
            remote_path: "onnx/model_int8.onnx",
            local_name: "model.onnx",
            sha256: "a13ec391ca99f49886694e12d3e800521f36d4267d7d448c34421c541a2baf50",
        },
        // tokenizer.json is byte-identical to the nomic bundle's (shared BERT
        // WordPiece vocab); the SHA matches accordingly.
        ModelFile {
            remote_path: "tokenizer.json",
            local_name: "tokenizer.json",
            sha256: "d241a60d5e8f04cc1b2b3e9ef7a4921b27bf526d9f6050ab90f9267a1f9e5c66",
        },
        ModelFile {
            remote_path: "config.json",
            local_name: "config.json",
            sha256: "d827779a72d27ae68cf878a6fc2e954542663fe21ca515d9f4783fc96be2d37e",
        },
        ModelFile {
            remote_path: "special_tokens_map.json",
            local_name: "special_tokens_map.json",
            sha256: "b6d346be366a7d1d48332dbc9fdf3bf8960b5d879522b7799ddba59e76237ee3",
        },
        ModelFile {
            remote_path: "tokenizer_config.json",
            local_name: "tokenizer_config.json",
            sha256: "0b29c7bfc889e53b36d9dd3e686dd4300f6525110eaa98c76a5dafceb2029f53",
        },
    ],
};

const BUNDLES: &[&ModelBundle] = &[&NOMIC_EMBED, &RERANKER];

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR not set by cargo"));
    for bundle in BUNDLES {
        let stage_dir = out_dir.join(bundle.local_dir);
        fs::create_dir_all(&stage_dir).unwrap_or_else(|e| {
            panic!("failed to create {}: {e}", stage_dir.display());
        });
        for file in bundle.files {
            let dest = stage_dir.join(file.local_name);
            ensure_file(&dest, bundle.base_url, file);
        }
    }
}

fn ensure_file(dest: &Path, base_url: &str, file: &ModelFile) {
    if let Ok(actual) = sha256_of(dest) {
        if actual.eq_ignore_ascii_case(file.sha256) {
            return;
        }
        println!(
            "cargo:warning=cached {} has unexpected hash {actual}; re-downloading",
            dest.display()
        );
        let _ = fs::remove_file(dest);
    }

    let url = format!("{base_url}/{}", file.remote_path);
    println!(
        "cargo:warning=memhub: downloading {} -> {}",
        url,
        dest.display()
    );

    let response = ureq::get(&url)
        .call()
        .unwrap_or_else(|e| panic!("failed to fetch {url}: {e}"));

    let capacity = response
        .header("Content-Length")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(0);
    let mut bytes: Vec<u8> = Vec::with_capacity(capacity);
    response
        .into_reader()
        .read_to_end(&mut bytes)
        .unwrap_or_else(|e| panic!("failed to read body of {url}: {e}"));

    let actual = hex_sha256(&bytes);
    if !actual.eq_ignore_ascii_case(file.sha256) {
        panic!(
            "{url} sha256 mismatch: expected {}, got {actual}",
            file.sha256
        );
    }

    let tmp = dest.with_extension("download");
    let mut f = fs::File::create(&tmp)
        .unwrap_or_else(|e| panic!("failed to create {}: {e}", tmp.display()));
    f.write_all(&bytes)
        .unwrap_or_else(|e| panic!("failed to write {}: {e}", tmp.display()));
    f.sync_all().ok();
    drop(f);
    fs::rename(&tmp, dest)
        .unwrap_or_else(|e| panic!("failed to rename into {}: {e}", dest.display()));
}

fn sha256_of(path: &Path) -> std::io::Result<String> {
    let mut f = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex_encode(hasher.finalize().as_slice()))
}

fn hex_sha256(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex_encode(hasher.finalize().as_slice())
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}
