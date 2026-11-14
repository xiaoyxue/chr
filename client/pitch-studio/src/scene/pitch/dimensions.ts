export function validateDimensions(length: number, width: number) {
  if (!Number.isFinite(length) || !Number.isFinite(width)) {
    throw new Error('请输入有效的球场长、宽。');
  }
  if (length < 90 || length > 120 || width < 45 || width > 90) {
    throw new Error('长度需为 90–120 米，宽度需为 45–90 米。');
  }
  if (length <= width) {
    throw new Error('球场长度必须大于宽度。');
  }
  return { length, width, area: Math.round(length * width * 100) / 100 };
}
