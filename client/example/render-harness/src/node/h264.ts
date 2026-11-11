// Reassembles H.264 RTP payloads (RFC 6184, packetization-mode 1) into Annex B access units.

const START_CODE = Buffer.from([0, 0, 0, 1]);
const STAP_A = 24;
const FU_A = 28;

export interface AccessUnit {
  /** Annex B bytes: start code before every NAL unit. */
  data: Buffer;
  nalTypes: number[];
  keyframe: boolean;
}

export class H264Depacketizer {
  #nals: Buffer[] = [];
  #fragment: Buffer[] | null = null;
  #lastSequence: number | null = null;
  #broken = false;
  /** Access units discarded because packets were missing. */
  dropped = 0;

  /** Feeds one RTP packet. Returns a complete access unit when `marker` ends one. */
  push(sequenceNumber: number, payload: Buffer, marker: boolean): AccessUnit | null {
    if (this.#lastSequence !== null && ((this.#lastSequence + 1) & 0xffff) !== sequenceNumber) {
      this.#broken = true;
      this.#fragment = null;
    }
    this.#lastSequence = sequenceNumber;
    this.#depacketize(payload);
    if (!marker) return null;

    const nals = this.#nals;
    const broken = this.#broken || this.#fragment !== null;
    this.#nals = [];
    this.#fragment = null;
    this.#broken = false;
    if (broken || nals.length === 0) {
      this.dropped += 1;
      return null;
    }
    const nalTypes = nals.map((nal) => (nal[0] ?? 0) & 0x1f);
    return {
      data: Buffer.concat(nals.flatMap((nal) => [START_CODE, nal])),
      nalTypes,
      keyframe: nalTypes.includes(5),
    };
  }

  #depacketize(payload: Buffer): void {
    const type = (payload[0] ?? 0) & 0x1f;
    if (type >= 1 && type <= 23) {
      this.#nals.push(payload);
    } else if (type === STAP_A) {
      let offset = 1;
      while (offset + 2 <= payload.length) {
        const size = payload.readUInt16BE(offset);
        offset += 2;
        if (size === 0 || offset + size > payload.length) {
          this.#broken = true;
          return;
        }
        this.#nals.push(payload.subarray(offset, offset + size));
        offset += size;
      }
    } else if (type === FU_A && payload.length >= 2) {
      const indicator = payload[0] ?? 0;
      const header = payload[1] ?? 0;
      const start = (header & 0x80) !== 0;
      const end = (header & 0x40) !== 0;
      if (start) {
        this.#fragment = [Buffer.from([(indicator & 0xe0) | (header & 0x1f)])];
      } else if (this.#fragment === null) {
        this.#broken = true;
        return;
      }
      this.#fragment?.push(payload.subarray(2));
      if (end && this.#fragment !== null) {
        this.#nals.push(Buffer.concat(this.#fragment));
        this.#fragment = null;
      }
    } else {
      this.#broken = true;
    }
  }
}
