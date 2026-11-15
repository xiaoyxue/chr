// A match's local placements (both spaces) as one document of the studio store,
// shared by the pitch and point-cloud editors so their saves never overwrite each
// other. Material files stay on the server; reading the placements loads them back.
import { StoreConflict, StoredDocument, readServerFile } from '../shared/studio-store.ts';
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
    const loaded = await Promise.all(ads.map(async (ad) => {
      if (!ad.materialPath) return ad;
      try { return { ...ad, material: await readServerFile(ad.materialPath, ad.materialName) }; }
      catch { missing.push(`${ad.name}（${ad.materialPath}）`); return ad; }
    }));
    return { ads: loaded, missing };
  }

  /** Replaces one space's placements, keeping the other space's as last read or saved. */
  write(matchId: string, ads: LocalAd[], space: AdSpace) {
    return this.save(matchId, (saved) => [...saved.filter((ad) => (ad.space ?? 'cloud') !== space), ...ads]);
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
