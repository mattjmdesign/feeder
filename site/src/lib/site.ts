export const REPO_URL = 'https://github.com/mattjmdesign/feeder';
export const RELEASES_URL = `${REPO_URL}/releases`;
export const DOWNLOAD_URL = `${REPO_URL}/releases/latest/download/Feeder.dmg`;
export const SITE_URL = 'https://mattjmdesign.github.io/feeder/';

export function asset(path: string): string {
  const base = import.meta.env.BASE_URL.replace(/\/$/, '');
  const clean = path.replace(/^\//, '');
  return `${base}/${clean}`;
}
