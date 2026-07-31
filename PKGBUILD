# Maintainer: VeigaPunk

pkgname=ufo-cli
pkgver=0.1.0
pkgrel=2
pkgdesc="Pure Rust UFO local rover CLI"
arch=('x86_64')
url="https://github.com/VeigaPunk/ufogrokbd"
license=('BSD-3-Clause')
depends=('glibc' 'gcc-libs' 'libgcc')
makedepends=('cargo')
options=('!debug')
source=()
sha256sums=()

build() {
  cargo build --release --locked --manifest-path "$startdir/ufo-cli/Cargo.toml"
}

package() {
  install -Dm755 "$startdir/ufo-cli/target/release/ufo" "$pkgdir/usr/bin/ufo"
  install -Dm644 "$startdir/README.md" "$pkgdir/usr/share/doc/$pkgname/README.md"
  install -Dm644 "$startdir/LICENSE" "$pkgdir/usr/share/licenses/$pkgname/LICENSE"
}
