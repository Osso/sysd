# Maintainer: Alessio Deiana <adeiana@gmail.com>
pkgname=sysd
pkgver=0.1.0
pkgrel=1
pkgdesc="Minimal systemd-compatible init system"
arch=('x86_64')
license=('MIT')
depends=('gcc-libs')
makedepends=('cargo')
backup=()
install=sysd.install

build() {
    cd "$startdir"
    cargo build --release
}

package() {
    cd "$startdir"

    # Binaries
    install -Dm755 target/release/sysd "$pkgdir/usr/bin/sysd"
    install -Dm755 target/release/sysdctl "$pkgdir/usr/bin/sysdctl"

    # Pacman hooks
    install -Dm644 hooks/sysd-daemon-reload.hook "$pkgdir/usr/share/libalpm/hooks/sysd-daemon-reload.hook"
    install -Dm755 hooks/sysd-daemon-reload "$pkgdir/usr/share/libalpm/scripts/sysd-daemon-reload"
}
