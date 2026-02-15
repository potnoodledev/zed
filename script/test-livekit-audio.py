#!/usr/bin/env python3
"""
Publish a 440 Hz sine-wave tone into a Zed channel's LiveKit room.

Usage:
    pip install livekit livekit-api PyJWT numpy asyncpg
    python script/test-livekit-audio.py --channel general

The script connects to the collab Postgres DB (localhost:5432) to look up the
LiveKit room name for the given channel.  If no room exists yet it creates one
via the LiveKit API.  It then joins the room and publishes synthetic audio
until you press Ctrl-C.

NOTE: Zed only plays audio from participants whose LiveKit identity matches a
user ID known to the collab server (i.e. a user who joined the room via collab
RPC).  By default, this script uses --identity 1 (the first seeded user,
"nathansobo").  For Zed to actually play the audio, that user must also be a
room participant in collab's DB.  The simplest way to hear the tone is to run
a second Zed instance signed in as a different user and join the same channel.
"""

import argparse
import asyncio
import signal
import sys
import time

import jwt
import numpy as np
from livekit import api, rtc


LIVEKIT_URL = "ws://localhost:7880"
LIVEKIT_API_KEY = "devkey"
LIVEKIT_API_SECRET = "secret"
DB_DSN = "postgres://postgres@localhost:5432/zed"

SAMPLE_RATE = 48000
NUM_CHANNELS = 1
TONE_HZ = 440
AMPLITUDE = 0.5
FRAME_DURATION_MS = 20
SAMPLES_PER_FRAME = SAMPLE_RATE * FRAME_DURATION_MS // 1000


def generate_token(room: str, identity: str) -> str:
    now = int(time.time())
    payload = {
        "iss": LIVEKIT_API_KEY,
        "sub": identity,
        "iat": now,
        "nbf": now,
        "exp": now + 3600,
        "video": {
            "room": room,
            "roomJoin": True,
            "canPublish": True,
            "canSubscribe": True,
        },
    }
    return jwt.encode(payload, LIVEKIT_API_SECRET, algorithm="HS256")


async def find_or_create_room(channel_name: str) -> str:
    try:
        import asyncpg
    except ImportError:
        sys.exit(
            "asyncpg is required to look up rooms from Postgres.\n"
            "Install it with:  pip install asyncpg\n"
            "Or pass --room <name> to skip the DB lookup."
        )

    connection = await asyncpg.connect(DB_DSN)
    try:
        row = await connection.fetchrow(
            """
            SELECT r.live_kit_room
              FROM rooms r
              JOIN channels c ON c.id = r.channel_id
             WHERE c.name = $1
             LIMIT 1
            """,
            channel_name,
        )
    finally:
        await connection.close()

    if row and row["live_kit_room"]:
        room_name = row["live_kit_room"]
        print(f"Found existing room '{room_name}' for channel '{channel_name}'")
        return room_name

    room_name = f"channel-{channel_name}"
    print(
        f"No room found for channel '{channel_name}', "
        f"creating '{room_name}' via LiveKit API"
    )
    lk_api = api.LiveKitAPI(LIVEKIT_URL, LIVEKIT_API_KEY, LIVEKIT_API_SECRET)
    try:
        await lk_api.room.create_room(api.CreateRoomRequest(name=room_name))
    finally:
        await lk_api.aclose()
    return room_name


async def publish_sine_wave(room_name: str, identity: str) -> None:
    token = generate_token(room_name, identity)

    lk_room = rtc.Room()

    stop = asyncio.Event()
    loop = asyncio.get_running_loop()
    for sig in (signal.SIGINT, signal.SIGTERM):
        loop.add_signal_handler(sig, stop.set)

    await lk_room.connect(LIVEKIT_URL, token)
    print(f"Connected to room '{room_name}' as identity '{identity}'")

    remote_participants = list(lk_room.remote_participants.values())
    if remote_participants:
        for participant in remote_participants:
            tracks = list(participant.track_publications.keys())
            print(f"  Remote participant: {participant.identity} (tracks: {tracks})")
    else:
        print("  No remote participants in the room yet")

    source = rtc.AudioSource(SAMPLE_RATE, NUM_CHANNELS)
    track = rtc.LocalAudioTrack.create_audio_track("sine-wave", source)
    options = rtc.TrackPublishOptions(source=rtc.TrackSource.SOURCE_MICROPHONE)
    await lk_room.local_participant.publish_track(track, options)
    print(f"Publishing {TONE_HZ} Hz sine wave — press Ctrl-C to stop")

    phase = 0.0
    phase_increment = 2.0 * np.pi * TONE_HZ / SAMPLE_RATE

    try:
        while not stop.is_set():
            indices = np.arange(SAMPLES_PER_FRAME)
            samples = AMPLITUDE * np.sin(phase + phase_increment * indices)
            phase += phase_increment * SAMPLES_PER_FRAME
            phase %= 2.0 * np.pi

            frame = rtc.AudioFrame.create(SAMPLE_RATE, NUM_CHANNELS, SAMPLES_PER_FRAME)
            frame_data = np.frombuffer(frame.data, dtype=np.int16)
            np.copyto(frame_data, (samples * 32767).astype(np.int16))

            await source.capture_frame(frame)
    finally:
        await lk_room.disconnect()
        print("Disconnected")


async def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    group = parser.add_mutually_exclusive_group(required=True)
    group.add_argument("--channel", help="Zed channel name to look up in Postgres")
    group.add_argument("--room", help="LiveKit room name (skip DB lookup)")
    parser.add_argument(
        "--identity",
        default="1",
        help="LiveKit participant identity (default: '1'). "
        "Zed expects a numeric collab user ID.",
    )
    args = parser.parse_args()

    room_name = args.room if args.room else await find_or_create_room(args.channel)
    await publish_sine_wave(room_name, args.identity)


if __name__ == "__main__":
    asyncio.run(main())
