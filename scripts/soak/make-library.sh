#!/bin/sh
# Generates a synthetic local music library under DIR for the memory soak:
# COUNT short mp3/flac tracks with unique tags, spread across ARTIST_COUNT artists
# and ALBUM_COUNT albums, a few multi-artist tags, year/genre/track/disc metadata,
# and an embedded cover on about half the albums. Runs jobs in parallel with xargs.
set -eu

DIR=${1:-/tmp/soak/music}
COUNT=${2:-11000}
ARTIST_COUNT=900
ALBUM_COUNT=1500
JOBS=${JOBS:-$(nproc)}

mkdir -p "$DIR/covers"
# A few distinct embedded covers: small squares plus a couple of larger 1200px ones.
ffmpeg -y -loglevel error -f lavfi -i "color=c=0x224488:size=300x300" -frames:v 1 "$DIR/covers/small-a.jpg"
ffmpeg -y -loglevel error -f lavfi -i "color=c=0x884422:size=300x300" -frames:v 1 "$DIR/covers/small-b.jpg"
ffmpeg -y -loglevel error -f lavfi -i "color=c=0x228844:size=1200x1200" -frames:v 1 "$DIR/covers/large-a.jpg"
ffmpeg -y -loglevel error -f lavfi -i "testsrc=size=1200x1200:rate=1" -frames:v 1 "$DIR/covers/large-b.jpg"

export DIR ARTIST_COUNT ALBUM_COUNT
seq 0 $((COUNT - 1)) | xargs -P "$JOBS" -I{} sh -c '
    i={}
    artist=$((i % ARTIST_COUNT))
    album=$((i % ALBUM_COUNT))
    track=$((i % 12 + 1))
    disc=$((i % 2 + 1))
    year=$((1980 + i % 45))
    genre=$((i % 8))
    if [ $((i % 17)) -eq 0 ]; then
        title_artist="Artist $artist & Artist $(((artist + 7) % ARTIST_COUNT))"
    else
        title_artist="Artist $artist"
    fi
    sub=$(printf "%03d" $album)
    dest="$DIR/album$sub"
    mkdir -p "$dest"
    # roughly half the albums carry one of the four covers, embedded as a picture
    cover_args=""
    case $((album % 4)) in
        0) cover_args="-i $DIR/covers/small-a.jpg -map 1 -c:v:1 mjpeg -disposition:v:1 attached_pic" ;;
        2) cover_args="-i $DIR/covers/large-b.jpg -map 1 -c:v:1 mjpeg -disposition:v:1 attached_pic" ;;
    esac
    # alternate mp3 and flac, 1.5 s of a quiet sine
    if [ $((i % 2)) -eq 0 ]; then
        out="$dest/track$(printf %02d $track).mp3"
        codec="libmp3lame -b:a 64k"
    else
        out="$dest/track$(printf %02d $track).flac"
        codec="flac"
    fi
    [ -f "$out" ] && exit 0
    # shellcheck disable=SC2086
    ffmpeg -y -loglevel error -f lavfi -i "sine=frequency=$((220 + i % 660)):duration=1.5" \
        $cover_args -map 0:a -c:a $codec \
        -metadata title="Track $i" \
        -metadata artist="$title_artist" \
        -metadata album="Album $album" \
        -metadata album_artist="Artist $((album % ARTIST_COUNT))" \
        -metadata track="$track" -metadata disc="$disc" \
        -metadata date="$year" -metadata genre="Genre$genre" \
        "$out"
'
echo "library ready: $(find "$DIR" -name '*.mp3' -o -name '*.flac' | wc -l) tracks in $DIR"
