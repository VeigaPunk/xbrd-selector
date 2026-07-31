# Maintainer: VeigaPunk

pkgname=xbrd-selector
pkgver=0.1.1
pkgrel=1
pkgdesc="Pure Rust xbrd-selector local rover CLI"
arch=('x86_64')
url="https://github.com/VeigaPunk/xbrd-selector"
license=('BSD-3-Clause')
depends=('glibc' 'gcc-libs' 'libgcc')
makedepends=('cargo')
options=('!debug')
source=()
sha256sums=()

build() {
  env -u RUSTFLAGS -u CARGO_ENCODED_RUSTFLAGS -u CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS \
    -u CFLAGS -u CXXFLAGS -u CPPFLAGS -u LDFLAGS \
    cargo build --release --locked --manifest-path "$startdir/ufo-cli/Cargo.toml"
}

package() {
  install -Dm755 "$startdir/ufo-cli/target/release/xbrd-selector" "$pkgdir/usr/bin/xbrd-selector"
  ln -s xbrd-selector "$pkgdir/usr/bin/ufo"
  install -Dm644 "$startdir/README.md" "$pkgdir/usr/share/doc/$pkgname/README.md"
  install -Dm644 "$startdir/LICENSE" "$pkgdir/usr/share/licenses/$pkgname/LICENSE"
}
