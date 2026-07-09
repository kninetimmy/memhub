// memhub build script
//
// Stages the bundled retrieval model files into OUT_DIR so the main crate
// can include them via include_bytes!. Two models are bundled:
//
//   * BGE-small-en-v1.5 — bi-encoder used by the M8 hybrid recall path.
//     ~127 MB. Source: BAAI/bge-small-en-v1.5@main, commit
//     5c38ec7c405ec4b44b94cc5a9bb96e735b38267a.
//
//   * ms-marco-MiniLM-L-6-v2 — cross-encoder re-ranker bundled by the
//     task-#21 work. ~22 MB (int8-quantized `model_int8.onnx`; issue #75
//     / Q18, decision 148 — was ~91 MB fp32). Source:
//     Xenova/ms-marco-MiniLM-L-6-v2@main (an ONNX export of
//     cross-encoder/ms-marco-MiniLM-L-6-v2). Selected over BGE-reranker-
//     v2-m3 in the bake-off (decisions 68–70): +17.7pp Recall@1 over
//     baseline, 15× faster than BGE-v2-m3, no keyword regressions.
//     BAAI publishes no quantized bge-small ONNX (verified against the
//     HF repo file tree), so the bi-encoder stays fp32 — only the
//     re-ranker is int8 here (partial adoption, issue #75).
//
// On a clean build this downloads ~150 MB total from Hugging Face (was
// ~218 MB before the int8 re-ranker swap). On
// rebuilds the staged files are reused if their SHA256 matches the
// pinned value. All hashes were computed locally over the downloaded
// bytes (or, for BGE-small's model.onnx, match the x-linked-etag
// returned by Hugging Face for the pinned commit).
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
    /// Hugging Face base URL (resolve/main).
    base_url: &'static str,
    files: &'static [ModelFile],
}

const BGE_SMALL: ModelBundle = ModelBundle {
    local_dir: "bge-small-en-v1.5",
    base_url: "https://huggingface.co/BAAI/bge-small-en-v1.5/resolve/main",
    files: &[
        ModelFile {
            remote_path: "onnx/model.onnx",
            local_name: "model.onnx",
            sha256: "828e1496d7fabb79cfa4dcd84fa38625c0d3d21da474a00f08db0f559940cf35",
        },
        ModelFile {
            remote_path: "tokenizer.json",
            local_name: "tokenizer.json",
            sha256: "d241a60d5e8f04cc1b2b3e9ef7a4921b27bf526d9f6050ab90f9267a1f9e5c66",
        },
        ModelFile {
            remote_path: "config.json",
            local_name: "config.json",
            sha256: "094f8e891b932f2000c92cfc663bac4c62069f5d8af5b5278c4306aef3084750",
        },
        ModelFile {
            remote_path: "special_tokens_map.json",
            local_name: "special_tokens_map.json",
            sha256: "b6d346be366a7d1d48332dbc9fdf3bf8960b5d879522b7799ddba59e76237ee3",
        },
        ModelFile {
            remote_path: "tokenizer_config.json",
            local_name: "tokenizer_config.json",
            sha256: "9261e7d79b44c8195c1cada2b453e55b00aeb81e907a6664974b4d7776172ab3",
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
        // tokenizer.json and special_tokens_map.json are byte-identical to
        // BGE-small's (shared BERT WordPiece vocab); SHAs match accordingly.
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

const BUNDLES: &[&ModelBundle] = &[&BGE_SMALL, &RERANKER];

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
