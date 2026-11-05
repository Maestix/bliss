#!/usr/bin/env bash
# Build bliss on Linux, after checking that everything the build needs is
# there — and offering to install what is not, with the distribution's own
# package manager.
#
#   ./build-linux.sh           check, offer to install what is missing, build
#   ./build-linux.sh --check   only say what is there and what is missing
#   ./build-linux.sh --yes     install what is missing without asking
#   ./build-linux.sh --debug   a debug build instead of a release one
#
# What a build needs: a C compiler and pkg-config; WebKitGTK 4.1, GTK 3 and
# libsoup 3 for the window; OpenSSL, which dioxus-desktop takes in through
# native-tls; libxdo, which muda links before bliss drops it (AGENTS.md §12);
# and Rust 1.88 or newer, which the locked dependencies ask for. Without any
# one of them the build stops halfway through with an error in some -sys
# crate's build script, which names a .pc file rather than a package.
set -euo pipefail

cd "$(dirname "$0")"

MIN_RUST="1.88"
check_only=0
assume_yes=0
profile="release"
for arg in "$@"; do
    case "$arg" in
        --check) check_only=1 ;;
        --yes | -y) assume_yes=1 ;;
        --debug) profile="debug" ;;
        -h | --help)
            sed -n '2,16p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            echo "Не знаю такого: $arg (есть --check, --yes, --debug)" >&2
            exit 2
            ;;
    esac
done

if [ -t 1 ]; then
    bold=$'\e[1m' green=$'\e[32m' red=$'\e[31m' yellow=$'\e[33m' plain=$'\e[0m'
else
    bold="" green="" red="" yellow="" plain=""
fi
ok() { echo "  ${green}✓${plain} $*"; }
missing() { echo "  ${red}✗${plain} $*"; }
note() { echo "  ${yellow}!${plain} $*"; }

ask() {
    [ "$assume_yes" = 1 ] && return 0
    [ -t 0 ] || return 1
    local answer
    read -r -p "$1 [Y/n] " answer
    case "$answer" in "" | [YyДд]*) return 0 ;; *) return 1 ;; esac
}

# --- the distribution -----------------------------------------------------

manager=""
for candidate in apt-get dnf pacman zypper; do
    if command -v "$candidate" >/dev/null 2>&1; then
        manager="$candidate"
        break
    fi
done

# Every package the build needs, by manager: the window's libraries, OpenSSL,
# libxdo, a compiler and pkg-config, and the tray's library and librsvg,
# which the build does not link but the window uses at run time.
case "$manager" in
    apt-get)
        packages=(build-essential pkg-config libwebkit2gtk-4.1-dev libgtk-3-dev
            libsoup-3.0-dev libssl-dev libxdo-dev libayatana-appindicator3-dev librsvg2-dev)
        install=(apt-get install -y)
        refresh=(apt-get update)
        ;;
    dnf)
        packages=(gcc gcc-c++ make pkgconf-pkg-config webkit2gtk4.1-devel gtk3-devel
            libsoup3-devel openssl-devel libxdo-devel libayatana-appindicator-gtk3-devel
            librsvg2-devel)
        install=(dnf install -y)
        refresh=()
        ;;
    pacman)
        packages=(base-devel pkgconf webkit2gtk-4.1 gtk3 libsoup3 openssl xdotool
            libayatana-appindicator librsvg)
        install=(pacman -S --needed --noconfirm)
        refresh=()
        ;;
    zypper)
        packages=(gcc gcc-c++ make pkg-config webkit2gtk3-devel gtk3-devel libsoup-devel
            libopenssl-devel xdotool-devel libayatana-appindicator3-devel librsvg-devel)
        install=(zypper install -y)
        refresh=()
        ;;
    *)
        packages=()
        install=()
        refresh=()
        ;;
esac

as_root() {
    if [ "$(id -u)" = 0 ]; then
        "$@"
    elif command -v sudo >/dev/null 2>&1; then
        sudo "$@"
    else
        echo "Нужны права администратора, а sudo нет: выполните от root:" >&2
        echo "  $*" >&2
        return 1
    fi
}

# --- what is there ----------------------------------------------------------

echo "${bold}Что нужно для сборки${plain}"
problems=0

if command -v cc >/dev/null 2>&1; then ok "компилятор C ($(cc --version | head -n1))"
else missing "компилятор C"; problems=1; fi

if command -v pkg-config >/dev/null 2>&1; then
    ok "pkg-config"
    for module in "webkit2gtk-4.1:WebKitGTK 4.1" "gtk+-3.0:GTK 3" "libsoup-3.0:libsoup 3" "openssl:OpenSSL"; do
        name="${module%%:*}" label="${module#*:}"
        if pkg-config --exists "$name"; then ok "$label $(pkg-config --modversion "$name")"
        else missing "$label (pkg-config: $name)"; problems=1; fi
    done
else
    missing "pkg-config — без него не проверить и библиотеки"
    problems=1
fi

# libxdo ships no .pc file: ask the linker for it.
if command -v cc >/dev/null 2>&1 &&
    echo 'int main(void){return 0;}' | cc -x c - -lxdo -o /dev/null >/dev/null 2>&1; then
    ok "libxdo"
else
    missing "libxdo (для сборки; в готовой программе её нет)"
    problems=1
fi

# Rust: rustup's newest stable when rustup is there, else whatever cargo is.
rust_ok=0
version_of() { "$@" --version 2>/dev/null | awk '{print $2}'; }
at_least() { [ "$(printf '%s\n%s\n' "$MIN_RUST" "$1" | sort -V | head -n1)" = "$MIN_RUST" ]; }
cargo_cmd=(cargo)
if command -v cargo >/dev/null 2>&1; then
    have="$(version_of rustc)"
    if [ -n "$have" ] && at_least "$have"; then
        ok "Rust $have"
        rust_ok=1
    elif command -v rustup >/dev/null 2>&1 && rustup run stable rustc --version >/dev/null 2>&1 &&
        at_least "$(rustup run stable rustc --version | awk '{print $2}')"; then
        ok "Rust $(rustup run stable rustc --version | awk '{print $2}') (stable из rustup; по умолчанию $have)"
        cargo_cmd=(cargo +stable)
        rust_ok=1
    else
        missing "Rust $MIN_RUST или новее (сейчас ${have:-неизвестно})"
        problems=1
    fi
else
    missing "Rust (cargo) — нужен $MIN_RUST или новее"
    problems=1
fi

if [ "$problems" = 0 ]; then
    echo "${green}Всё на месте.${plain}"
elif [ "$check_only" = 1 ]; then
    echo
    if [ "${#packages[@]}" -gt 0 ]; then
        echo "Поставить библиотеки: ${bold}sudo ${install[*]} ${packages[*]}${plain}"
    fi
    if [ "$rust_ok" = 0 ]; then
        if [ "$manager" = pacman ]; then echo "Rust: ${bold}sudo pacman -S rust${plain}"
        else echo "Rust: ${bold}curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh${plain}"; fi
    fi
    exit 1
fi
[ "$check_only" = 1 ] && exit 0

# --- installing what is missing ------------------------------------------

if [ "$problems" = 1 ]; then
    echo
    if [ "${#packages[@]}" -eq 0 ]; then
        echo "${red}Не узнал пакетный менеджер${plain} (ищу apt-get, dnf, pacman, zypper)."
        echo "Поставьте сами: компилятор C, pkg-config, WebKitGTK 4.1, GTK 3, libsoup 3,"
        echo "OpenSSL и libxdo в версиях для разработки (-dev / -devel), и Rust $MIN_RUST+."
        exit 1
    fi
    echo "Для ${bold}$manager${plain} это пакеты: ${packages[*]}"
    if ask "Поставить их сейчас (спросит пароль администратора)?"; then
        if [ "${#refresh[@]}" -gt 0 ]; then as_root "${refresh[@]}"; fi
        as_root "${install[@]}" "${packages[@]}"
    else
        echo "Тогда сами: sudo ${install[*]} ${packages[*]}"
        exit 1
    fi
    if [ "$rust_ok" = 0 ]; then
        if [ "$manager" = pacman ] && ! command -v rustup >/dev/null 2>&1; then
            # Arch's own Rust is current; no installer from the web needed.
            if ask "Поставить Rust из репозитория Arch (pacman -S rust)?"; then
                as_root pacman -S --needed --noconfirm rust
            else
                exit 1
            fi
        elif command -v rustup >/dev/null 2>&1; then
            if ask "Поставить свежий Rust через rustup?"; then
                rustup toolchain install stable --profile minimal
                cargo_cmd=(cargo +stable)
            else
                exit 1
            fi
        elif ask "Rust нет или он старый. Поставить rustup (официальный установщик Rust)?"; then
            curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
            # shellcheck disable=SC1091
            . "$HOME/.cargo/env"
            cargo_cmd=(cargo +stable)
        else
            echo "Тогда сами: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
            exit 1
        fi
    fi
    echo
    exec "$0" "$@"
fi

# --- building ---------------------------------------------------------------

# In WSL, building from the Windows side: a target dir on Linux's own file
# system, and the Windows user's cargo config (kache) switched off, which
# cargo finds by walking up from /mnt/c (AGENTS.md §3, trap 11).
case "$PWD" in
    /mnt/*)
        export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/bliss-target}"
        export RUSTC_WRAPPER="" CARGO_BUILD_RUSTC_WRAPPER=""
        note "сборка с диска Windows: результат в $CARGO_TARGET_DIR"
        ;;
esac

echo
echo "${bold}Собираю bliss ($profile)…${plain}"
if [ "$profile" = release ]; then
    "${cargo_cmd[@]}" build --release --locked -p bliss
else
    "${cargo_cmd[@]}" build --locked -p bliss
fi

target="${CARGO_TARGET_DIR:-target}/$profile/bliss"
echo
echo "${green}Готово:${plain} $target ($(du -h "$target" | cut -f1))"
echo "Для режима TUN (весь компьютер через bliss) программе нужно право на сеть,"
echo "заново после каждой сборки: ${bold}sudo setcap cap_net_admin+ep $target${plain}"
