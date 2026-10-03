#!/bin/bash
# Builds the ffmpeg that ships inside Interview Coach.app, so users never install it: a minimal,
# audio-only, LGPL build of the pinned FFmpeg release (signature verified once against FFmpeg's
# release key FCF986EA15E6E293A5644F10B4322F04D67658D8; its SHA-256 is pinned here). It covers
# exactly what ic asks of ffmpeg (src/audio.rs): decode recordings and imports (WAV, M4A/MP4/MOV,
# MP3, FLAC, AIFF, CAF, Ogg, WebM/Opus) to 16 kHz mono FLAC or raw f32, and mix tracks into an AAC
# listening copy. Video tracks are skipped (-vn), so no video decoders are built.
#
# Prints the directory holding bin/ffmpeg, its LGPL license and BUILD.txt (source and options).
# Later runs reuse the verified build.
set -euo pipefail
project_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
version=9.0.2
checksum=8c3850283eb25fa026482078a04051e0be17347b09ef81a0849bec15a96e002e
url="https://ffmpeg.org/releases/ffmpeg-$version.tar.xz"
directory="$project_root/mac/vendor/ffmpeg-$version"
[[ -f "$directory/.built" ]] && { printf '%s\n' "$directory"; exit 0; }

options=(
    --disable-everything --disable-autodetect --disable-network --disable-doc --disable-debug
    --disable-ffplay --disable-ffprobe --disable-avdevice --disable-swscale
    --enable-ffmpeg --enable-avformat --enable-avcodec --enable-avfilter --enable-swresample
    --enable-protocol=file,pipe
    --enable-demuxer=mov,mp3,wav,aiff,flac,ogg,matroska,caf,aac
    --enable-decoder=aac,aac_latm,mp3,mp3float,flac,alac,opus,vorbis,pcm_s16le,pcm_s16be,pcm_s24le,pcm_s24be,pcm_s32le,pcm_s32be,pcm_f32le,pcm_f32be,pcm_u8,pcm_alaw,pcm_mulaw
    --enable-parser=aac,aac_latm,flac,mpegaudio,opus,vorbis
    --enable-encoder=flac,aac,pcm_f32le,pcm_s16le
    --enable-muxer=flac,ipod,mp4,pcm_f32le,wav
    --enable-filter=aresample,aformat,anull,amix,abuffer,abuffersink
    --extra-cflags=-mmacosx-version-min=14.4 --extra-ldflags=-mmacosx-version-min=14.4
)

mkdir -p "$project_root/mac/vendor"
work="$(mktemp -d "$project_root/mac/vendor/ffmpeg-build.XXXXXX")"
trap 'rm -rf "$work"' EXIT
curl -fsSL --retry 3 -o "$work/ffmpeg.tar.xz" "$url"
if ! printf '%s  %s\n' "$checksum" "$work/ffmpeg.tar.xz" | shasum -a 256 -c - >/dev/null; then
    printf 'FFmpeg %s did not match its pinned checksum.\n' "$version" >&2
    exit 1
fi
tar -xf "$work/ffmpeg.tar.xz" -C "$work"
source_dir="$work/ffmpeg-$version"
printf 'Building ffmpeg %s (audio only, LGPL); this takes a few minutes once…\n' "$version" >&2
# A neutral prefix: `ffmpeg -version` prints the configure line, which shouldn't carry a local path.
(cd "$source_dir" && ./configure --prefix=/opt/interview-coach/ffmpeg "${options[@]}" >"$work/configure.log" 2>&1) \
    || { tail -30 "$work/configure.log" >&2; exit 1; }
(cd "$source_dir" && make -j"$(sysctl -n hw.ncpu)" ffmpeg >"$work/make.log" 2>&1) \
    || { tail -30 "$work/make.log" >&2; exit 1; }

binary="$source_dir/ffmpeg"
[[ "$(lipo -archs "$binary")" = arm64 ]] || { printf '%s\n' 'Expected an arm64 ffmpeg.' >&2; exit 1; }
if otool -L "$binary" | tail -n +2 | grep -vqE '^\s*/(usr/lib|System/Library)/'; then
    printf '%s\n' 'The bundled ffmpeg must link only system libraries:' >&2
    otool -L "$binary" >&2
    exit 1
fi
if "$binary" -hide_banner -L 2>/dev/null | grep -q 'GNU General Public License'; then
    printf '%s\n' 'The bundled ffmpeg must be an LGPL build.' >&2
    exit 1
fi

rm -rf "$directory"
mkdir -p "$directory/bin"
cp "$binary" "$directory/bin/ffmpeg"
cp "$source_dir/COPYING.LGPLv2.1" "$directory/LICENSE.txt"
{
    printf 'FFmpeg %s, built unmodified from %s\n' "$version" "$url"
    printf 'SHA-256 %s\n' "$checksum"
    printf 'Licensed under the GNU LGPL v2.1 or later (LICENSE.txt). Source: https://ffmpeg.org/download.html\n'
    printf 'configure %s\n' "${options[*]}"
} > "$directory/BUILD.txt"
touch "$directory/.built"
printf '%s\n' "$directory"
