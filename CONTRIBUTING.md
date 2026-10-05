# Berkontribusi ke speq-cli

`speq` adalah CLI Rust (edition 2024, minimal Rust `1.89`). Panduan ini berisi alur kerja kontribusi; penggunaan dan perilaku sinkronisasi ada di [README.md](README.md), proses rilis di [docs/releasing.md](docs/releasing.md).

> **Merge ke `main` bisa langsung merilis.** Setiap push ke `main` menjalankan `release.yml`, yang menghitung versi dari pesan commit lalu mem-publish GitHub Release tanpa persetujuan manual. Pesan commit Anda menentukan versi, jadi tulislah dengan benar.

## Menyiapkan lingkungan

```bash
rustup show            # rust-toolchain.toml memasang stable + rustfmt + clippy
cargo build
cargo run -- --help
```

Alat tambahan untuk memeriksa seperti CI: `cargo-deny` (`cargo install cargo-deny --locked`), `shellcheck`, dan `python3`.

Untuk menjalankan CLI melawan API dan Web lokal, ikuti `local-development.md` di workspace docs (bukan di repo ini) dan set `export SPEQ_API_URL=http://localhost:8787`.

## Perintah harian

```bash
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo deny check advisories bans licenses sources
```

Untuk perubahan di `scripts/` atau installer:

```bash
sh -n scripts/install.sh && shellcheck -S warning scripts/install.sh
python3 -m py_compile scripts/*.py
python3 scripts/test_release_scripts.py
```

CI menjalankan `fmt`, `clippy`, dan `test` di macOS (arm64), Linux (musl), dan Windows. Jangan menulis kode atau tes yang hanya benar di satu OS (separator path, izin file, simbol link).

## Struktur

```text
src/main.rs, cli.rs, commands.rs   titik masuk, definisi argumen, dan handler perintah
src/api.rs                         client HTTP ke API Speq
src/session.rs, credentials.rs     login, token, dan penyimpanan di credential store OS
src/workspace.rs, snapshot.rs      sinkronisasi tiga arah, lock, manifest, baseline
src/paths.rs, fsx.rs, hashing.rs   path aman, tulis atomik, hash blob Git
src/upgrade.rs, agent.rs           self-upgrade dan job refresh latar belakang
src/output.rs, error.rs            keluaran manusia/JSON dan kode keluar
tests/                             tes integrasi melawan API palsu (tests/support/)
scripts/                           installer dan skrip rilis (Python)
docs/releasing.md                  proses rilis
```

## Aturan yang wajib diikuti

- **Keamanan sinkronisasi** (lihat README, "How sync stays safe"): tidak ada opsi force atau buang-lokal, `pull` tidak menimpa kerja lokal, tulis lewat file sementara lalu rename atomik, symlink dan path di luar `specs/` tidak pernah diikuti, dan `push` tidak pernah di-retry otomatis. Perubahan yang melonggarkan ini perlu diskusi dulu.
- **Kredensial**: token hanya di credential store OS. Log tidak boleh memuat token atau isi dokumen; `SPEQ_LOG=debug` hanya menampilkan method, path, status, dan request ID. Tes tidak boleh menyentuh credential store, scheduler, atau jaringan sungguhan.
- **Kode keluar** (0–7, tabel di README) adalah kontrak untuk skrip dan agen. Jangan mengubah artinya; kasus baru memakai kode yang sudah ada, atau didiskusikan dulu.
- **Keluaran `--json`** harus tetap stabil: error dalam JSON di stderr. Menambah field boleh; mengubah atau menghapus field adalah perubahan breaking.
- **Otorisasi** selalu diputuskan API. Cache capability hanya petunjuk tampilan dan tidak boleh dipakai untuk mengizinkan tulis.
- **Lint**: `unsafe_code` di-`forbid` dan semua lint `clippy::all` bersifat `deny` (`Cargo.toml`), jadi warning clippy menggagalkan build. Kegagalan yang bisa datang dari input, I/O, atau jaringan dikembalikan sebagai error bermakna lewat `error.rs`, bukan `unwrap`/`expect`. `expect` hanya untuk invariant yang tidak mungkin gagal, dengan pesan yang menjelaskannya.
- **MSRV**: jangan memakai fitur yang membutuhkan Rust lebih baru dari `rust-version` di `Cargo.toml`; job `msrv` akan gagal. Menaikkan MSRV perlu alasan.

## Dependency

- Tambah dependency hanya bila perlu, dan pastikan `cargo deny check advisories bans licenses sources` lulus (konfigurasi di `deny.toml`). `Cargo.lock` di-commit dan CI memakai `--locked`.
- Dependabot membuka PR mingguan untuk crate dan GitHub Actions; tinjau seperti PR biasa.
- Repo ini harus tetap **publik** karena installer mengunduh rilis secara anonim. Jangan commit secret, token, URL internal, atau data pelanggan.

## Tes

Setiap perubahan perilaku harus disertai tes baru atau tes yang diperbarui, termasuk bug fix (tes yang gagal sebelum perbaikan).

- **Unit**: modul `#[cfg(test)]` di file terkait untuk logika murni (path, hashing, parsing).
- **Integrasi** (`tests/`): menjalankan library dan binary melawan API palsu stateful di `tests/support/` yang mengikuti kontrak di `speq-api/contract/`. Bila kontrak API berubah, perbarui API palsu dan tesnya bersamaan.
- Uji jalur gagalnya: sesi kedaluwarsa (exit 3), konflik (exit 5), izin ditolak (exit 6), input tidak valid (exit 7), jaringan putus (exit 4); jangan hanya jalur sukses.
- Skrip rilis dan installer punya tes sendiri di `scripts/test_release_scripts.py`.

## Commit dan pull request

Format `<type>: <ringkasan>` dengan scope opsional, misalnya `feat(cli): resolve spec documents under epics/ path`. Subject maksimal 72 karakter, kata kerja aktif, tanpa titik penutup; body hanya bila perlu menjelaskan alasan. Gunakan identitas Git developer yang sebenarnya.

Type menentukan rilis (`scripts/next_version.py`):

| Commit sejak tag terakhir | Hasil |
| --- | --- |
| `feat!: ...` atau footer `BREAKING CHANGE:` | major (minor selama versi `0.x`) |
| minimal satu `feat` | minor |
| hanya `fix`, `perf`, `refactor` | patch |
| hanya `docs`, `chore`, `test`, `ci`, `build`, `style`, `revert` | **tidak ada rilis** |

Pilih type sesuai dampak ke pengguna. Perubahan kode yang tidak boleh memicu rilis memakai `chore:` atau `docs:`. Bila PR di-squash, subject hasil squash yang membawa type-nya. Versi di `Cargo.toml` hanya placeholder dan **tidak diubah manual**; CI yang menempelkan versi saat rilis.

Checklist PR:

- [ ] `fmt`, `clippy`, `test`, dan `cargo deny` lulus di lokal
- [ ] tes baru/diperbarui untuk perilaku yang berubah
- [ ] README diperbarui bila perintah, flag, kode keluar, atau perilaku sinkronisasi berubah
- [ ] type commit sudah sesuai dampak rilis yang diinginkan
- [ ] tidak ada secret atau data pribadi di diff

CI pada PR menjalankan `test` (3 OS), `msrv`, `installers`, `supply-chain` (cargo-deny), dan `secrets` (gitleaks atas seluruh history). Semuanya harus hijau sebelum merge.
