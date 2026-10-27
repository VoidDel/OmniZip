// One-off generator: writes themes/builtin/*.json from the palette table below (taken from the
// approved prototype). Kept in the repo so palettes can be regenerated consistently.
import { writeFileSync, mkdirSync } from 'node:fs';
const T = {
  aurora: [{ 'zh-CN': '晨光', 'zh-TW': '晨光', 'en-US': 'Aurora', 'ja-JP': 'オーロラ' }, 'light', 0, ['#F5F7FB','#FFFFFF','#FFFFFF','#EEF2F8','#1B2230','#5B6577','#DCE2EC','#2F6FED','#1E9E63','#D98A0B','#D8434E','#2A8CD6']],
  midnight: [{ 'zh-CN': '午夜', 'zh-TW': '午夜', 'en-US': 'Midnight', 'ja-JP': 'ミッドナイト' }, 'dark', 0, ['#0F1115','#161922','#1C2029','#12151C','#E6E8EE','#A3A9B7','#2A3040','#5B8CFF','#3DD68C','#F5B544','#FF6B6B','#58B7FF']],
  graphite: [{ 'zh-CN': '石墨', 'zh-TW': '石墨', 'en-US': 'Graphite', 'ja-JP': 'グラファイト' }, 'dark', 0, ['#121212','#1A1A1A','#222222','#161616','#EDEDED','#A8A8A8','#2C2C2C','#8AB4F8','#6CCB8B','#E8B64A','#F28B82','#7FC2F5']],
  paper: [{ 'zh-CN': '纸张', 'zh-TW': '紙張', 'en-US': 'Paper', 'ja-JP': 'ペーパー' }, 'light', 0, ['#F7F3EC','#FFFDF9','#FFFFFF','#F0EAE0','#2B2620','#6B6259','#E3DBCE','#B8641B','#4E8F4A','#C48A12','#C04A3F','#3C78A8']],
  nord: [{ 'zh-CN': '北极', 'zh-TW': '北極', 'en-US': 'Nord', 'ja-JP': 'Nord' }, 'dark', 0, ['#2E3440','#3B4252','#434C5E','#2B303B','#ECEFF4','#B8C0CE','#4C566A','#88C0D0','#A3BE8C','#EBCB8B','#BF616A','#81A1C1']],
  'nord-light': [{ 'zh-CN': '北极 · 浅', 'zh-TW': '北極 · 淺', 'en-US': 'Nord Light', 'ja-JP': 'Nord ライト' }, 'light', 0, ['#ECEFF4','#FFFFFF','#FFFFFF','#E5E9F0','#2E3440','#4C566A','#D8DEE9','#5E81AC','#6F9F5A','#B08A2E','#BF616A','#5E81AC']],
  'catppuccin-mocha': [{ 'zh-CN': '卡布奇诺 · 摩卡', 'zh-TW': '卡布奇諾 · 摩卡', 'en-US': 'Catppuccin Mocha', 'ja-JP': 'Catppuccin モカ' }, 'dark', 0, ['#1E1E2E','#181825','#313244','#11111B','#CDD6F4','#A6ADC8','#45475A','#89B4FA','#A6E3A1','#F9E2AF','#F38BA8','#74C7EC']],
  'catppuccin-latte': [{ 'zh-CN': '卡布奇诺 · 拿铁', 'zh-TW': '卡布奇諾 · 拿鐵', 'en-US': 'Catppuccin Latte', 'ja-JP': 'Catppuccin ラテ' }, 'light', 0, ['#EFF1F5','#FFFFFF','#FFFFFF','#E6E9EF','#4C4F69','#6C6F85','#CCD0DA','#1E66F5','#40A02B','#DF8E1D','#D20F39','#209FB5']],
  'solarized-dark': [{ 'zh-CN': '日光 · 深', 'zh-TW': '日光 · 深', 'en-US': 'Solarized Dark', 'ja-JP': 'Solarized ダーク' }, 'dark', 0, ['#002B36','#073642','#0B3F4D','#00252F','#EEE8D5','#93A1A1','#1C4A56','#268BD2','#859900','#B58900','#DC322F','#2AA198']],
  'solarized-light': [{ 'zh-CN': '日光 · 浅', 'zh-TW': '日光 · 淺', 'en-US': 'Solarized Light', 'ja-JP': 'Solarized ライト' }, 'light', 0, ['#FDF6E3','#FFFCF2','#FFFFFF','#EEE8D5','#073642','#586E75','#E0D8C0','#268BD2','#859900','#B58900','#DC322F','#2AA198']],
  'high-contrast-light': [{ 'zh-CN': '高对比 · 浅', 'zh-TW': '高對比 · 淺', 'en-US': 'High Contrast Light', 'ja-JP': 'ハイコントラスト ライト' }, 'light', 1, ['#FFFFFF','#FFFFFF','#FFFFFF','#FFFFFF','#000000','#1A1A1A','#000000','#0000D6','#006B2E','#8A5A00','#B00020','#004C99']],
  'high-contrast-dark': [{ 'zh-CN': '高对比 · 深', 'zh-TW': '高對比 · 深', 'en-US': 'High Contrast Dark', 'ja-JP': 'ハイコントラスト ダーク' }, 'dark', 1, ['#000000','#000000','#0A0A0A','#000000','#FFFFFF','#E6E6E6','#FFFFFF','#FFD400','#3DFF9A','#FFC400','#FF5C5C','#4FC3FF']],
};
const keys = ['bg.canvas','bg.surface','bg.elevated','bg.sidebar','fg.primary','fg.secondary','border.default','accent','success','warning','danger','info'];
const dir = new URL('../themes/builtin/', import.meta.url);
mkdirSync(dir, { recursive: true });
for (const [id, [name, appearance, hc, values]] of Object.entries(T)) {
  const colors = Object.fromEntries(keys.map((k, i) => [k, values[i]]));
  const theme = {
    $schema: '../schema/theme-1.json',
    schemaVersion: 1,
    id,
    name,
    author: id.startsWith('nord') ? 'Nord palette (MIT) · OmniZip' : id.startsWith('catppuccin') ? 'Catppuccin palette (MIT) · OmniZip' : id.startsWith('solarized') ? 'Solarized palette (MIT) · OmniZip' : 'OmniZip',
    appearance,
    highContrast: !!hc,
    colors,
    effects: { material: hc ? 'none' : 'auto' },
    icons: { fileIconTheme: 'duotone', typeColors: hc ? 'none' : 'muted' },
  };
  writeFileSync(new URL(`${id}.json`, dir), JSON.stringify(theme, null, 2) + '\n');
}
console.log(Object.keys(T).length, 'themes written');
