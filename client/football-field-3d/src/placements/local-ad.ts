import type { FieldDimensions } from '../shared/types.ts';

export type Vector3Tuple = [number, number, number];
export type AdSpace = 'pitch' | 'cloud';
export interface AdPlane {
  origin: Vector3Tuple;
  u: Vector3Tuple;
  v: Vector3Tuple;
  width: number;
  height: number;
}
export interface LocalAd {
  id: string;
  name: string;
  type: '2D' | '3D';
  space?: AdSpace;
  cloudId?: string;
  plane: AdPlane;
  normalSign: number;
  offset: number;
  scale: number;
  yaw: number;
  visible: boolean;
  material?: File;
}

function valid(value: unknown): value is LocalAd {
  if (!value || typeof value !== 'object') return false;
  const ad = value as LocalAd;
  const vector = (point: number[]) => Array.isArray(point) && point.length === 3 && point.every((x) => Number.isFinite(x) && Math.abs(x) <= 1e6);
  return typeof ad.id === 'string' && typeof ad.name === 'string'
    && (ad.space === 'pitch' ? ad.cloudId === undefined : (ad.space === undefined || ad.space === 'cloud') && typeof ad.cloudId === 'string')
    && ['2D', '3D'].includes(ad.type) && typeof ad.visible === 'boolean' && Boolean(ad.plane)
    && vector(ad.plane.origin) && vector(ad.plane.u) && vector(ad.plane.v)
    && [ad.plane.u, ad.plane.v].every((axis) => Math.abs(axis.reduce((sum, x) => sum + x * x, 0) - 1) < 1e-5)
    && Math.abs(ad.plane.u.reduce((sum, x, i) => sum + x * ad.plane.v[i], 0)) < 1e-5
    && [ad.plane.width, ad.plane.height, ad.scale].every((x) => Number.isFinite(x) && x >= 0.01 && x <= 1000)
    && Number.isFinite(ad.offset) && Math.abs(ad.offset) <= 1000
    && Number.isFinite(ad.yaw) && Math.abs(ad.yaw) <= 360
    && [1, -1].includes(ad.normalSign) && (ad.material === undefined || ad.material instanceof File);
}

export function validatePitchAd(ad: LocalAd, field: FieldDimensions) {
  const { origin, u, v, width, height } = ad.plane;
  if (u[2] !== 0 || v[2] !== 0 || (ad.type === '2D' && origin[2] !== 0) || ad.offset !== 0 || ad.normalSign !== -1) {
    throw new Error('球场广告的底面应平行地面；2D 广告必须贴地，3D 高度使用 Z 调整。');
  }
  for (const [a, b] of [[0, 0], [width, 0], [0, height], [width, height]]) {
    if (Math.abs(origin[0] + a * u[0] + b * v[0]) > field.length / 2 + 1e-6
      || Math.abs(origin[1] + a * u[1] + b * v[1]) > field.width / 2 + 1e-6) {
      throw new Error('广告位底面超出球场，请调整位置或缩小尺寸。');
    }
  }
}

export class LocalAdRepository {
  private database?: Promise<IDBDatabase>;
  private open() {
    return this.database ??= new Promise<IDBDatabase>((resolve, reject) => {
      const request = indexedDB.open('liveshow.local-advertising.v1', 1);
      request.onupgradeneeded = () => request.result.createObjectStore('matches');
      request.onsuccess = () => resolve(request.result);
      request.onerror = () => reject(new Error(`无法打开本地广告素材存储：${request.error?.message}`));
      request.onblocked = () => reject(new Error('本地广告存储被其他页面阻塞，请关闭旧页面后重试。'));
    });
  }
  async read(id: string): Promise<LocalAd[]> {
    const database = await this.open();
    return new Promise((resolve, reject) => {
      const request = database.transaction('matches').objectStore('matches').get(id);
      request.onerror = () => reject(new Error(`读取本地广告失败：${request.error?.message}`));
      request.onsuccess = () => {
        const ads: unknown = request.result ?? [];
        if (!Array.isArray(ads) || ads.length > 200 || !ads.every(valid) || new Set(ads.map((ad) => ad.id)).size !== ads.length) {
          reject(new Error('本地广告数据损坏，未覆盖原记录。'));
        } else resolve(ads);
      };
    });
  }
  async write(id: string, ads: LocalAd[], space: AdSpace) {
    if (ads.length > 200 || !ads.every((ad) => valid(ad) && (ad.space ?? 'cloud') === space)) throw new Error('本地广告参数无效或超过 200 个。');
    const database = await this.open();
    return new Promise<void>((resolve, reject) => {
      const transaction = database.transaction('matches', 'readwrite');
      const store = transaction.objectStore('matches');
      const request = store.get(id);
      let validationError: Error;
      request.onsuccess = () => {
        const saved: unknown = request.result ?? [];
        if (!Array.isArray(saved) || !saved.every(valid)) {
          validationError = new Error('本地广告数据损坏，未覆盖原记录。');
        } else {
          // Merge inside one transaction so the independent editors cannot overwrite each other.
          const merged = [...saved.filter((ad) => (ad.space ?? 'cloud') !== space), ...ads];
          if (merged.length > 200 || new Set(merged.map((ad) => ad.id)).size !== merged.length) {
            validationError = new Error('本地广告超过 200 个或包含重复 ID，未覆盖原记录。');
          } else store.put(merged, id);
        }
        if (validationError) transaction.abort();
      };
      transaction.oncomplete = () => resolve();
      transaction.onabort = () => reject(validationError ?? new Error(`本地广告保存失败（可能存储空间不足）：${transaction.error?.message}`));
      transaction.onerror = () => reject(new Error(`本地广告保存失败：${transaction.error?.message}`));
    });
  }

  async remove(id: string) {
    const database = await this.open();
    return new Promise<void>((resolve, reject) => {
      const transaction = database.transaction('matches', 'readwrite');
      transaction.objectStore('matches').delete(id);
      transaction.oncomplete = () => resolve();
      transaction.onabort = transaction.onerror = () => reject(new Error(`删除本地广告及素材失败：${transaction.error?.message}`));
    });
  }
}
