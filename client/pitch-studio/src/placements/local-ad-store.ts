// A match's local placements (both spaces) as one document of the studio store,
// shared by the pitch and point-cloud editors so their saves never overwrite each
// other. Material files stay on the server; reading the placements loads them back.
import { StoreConflict, StoredDocument, readServerFile, serverSha256 } from '../shared/studio-store.ts';
import { materialFacts } from '../render/send-materials.ts';
import { glbBounds } from './local-ad-scene.ts';
import { storedLocalAd, upgradeLocalAd, validateStoredLocalAds, type AdSpace, type LocalAd } from './local-ad.ts';

export class LocalAdStore {
  private documents = new Map<string, StoredDocument<LocalAd[]>>();
  /** Another page changed a match's placements; the editors should read them again. */
  onConflict?: (matchId: string) => void;

  private document(matchId: string) {
    let document = this.documents.get(matchId);
    if (!document) this.documents.set(matchId, document = new StoredDocument(`matches/${encodeURIComponent(matchId)}/local-ads`, validateStoredLocalAds));
    return document;
  }

  /** The match's placements with their materials read back; `missing` names the materials that could not be read. */
  async read(matchId: string): Promise<{ ads: LocalAd[]; missing: string[] }> {
    const ads = ((await this.document(matchId).load()) ?? []).map(upgradeLocalAd), missing: string[] = [];
    const noted = new Map<string, { path: string; facts: Partial<LocalAd> }>();
    const loaded = await Promise.all(ads.map(async (ad) => {
      if (!ad.materialPath) return ad;
      let material: File;
      try { material = await readServerFile(ad.materialPath, ad.materialName); }
      catch { missing.push(`${ad.name}（${ad.materialPath}）`); return ad; }
      if (ad.materialSha256 && (ad.type !== '3D' || ad.materialBounds)) return { ...ad, material };
      // Placements from before materials were noted get their facts once, from the file just read.
      const facts = await serverSha256(ad.materialPath).then((sha256) => materialFacts(ad.type, material, sha256, { bounds: glbBounds }))
        .catch(() => ({}));
      if ('materialSha256' in facts) noted.set(ad.id, { path: ad.materialPath, facts });
      return { ...ad, material, ...facts };
    }));
    if (noted.size) await this.save(matchId, (saved) => saved.map((ad) => noted.get(ad.id)?.path === ad.materialPath
      ? { ...ad, ...noted.get(ad.id).facts } : ad)).catch((error: unknown) => console.warn('[local-ads] material facts not saved', error));
    return { ads: loaded, missing };
  }

  /** Replaces one space's placements, keeping the other space's as last read or saved. */
  write(matchId: string, ads: LocalAd[], space: AdSpace) {
    return this.save(matchId, (saved) => [...saved.filter((ad) => (ad.space ?? 'cloud') !== space), ...ads]);
  }

  /**
   * Adds what `change` finds missing, such as a package's tifo flags, to the stored placements: after
   * another page's save or a removed and re-imported match it reads them again and applies `change` once more.
   */
  merge(matchId: string, change: (saved: LocalAd[]) => LocalAd[]) {
    return this.document(matchId).update((saved) => change(saved ?? []).map(storedLocalAd));
  }

  /** Changes all of the match's placements at once, such as an import, from the copy last read or saved. */
  save(matchId: string, change: (saved: LocalAd[]) => LocalAd[]) {
    return this.document(matchId).save((saved) => change(saved ?? []).map(storedLocalAd)).catch((error: unknown) => {
      if (!(error instanceof StoreConflict)) throw error;
      // After the failed save has told the operator, so reloading does not hide that message.
      setTimeout(() => this.onConflict?.(matchId));
      throw new Error('另一个页面修改了本场次的本地广告位，已载入最新版本；刚才的修改未保存，请重新操作。');
    });
  }
}

export const localAdStore = new LocalAdStore();
