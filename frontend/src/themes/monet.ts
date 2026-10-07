/// 莫奈取色（Material You / Monet）：从一张图里提炼出整套面板配色。
///
/// 三步，全部由 Google 的 material-color-utilities 完成：
///   1. 量化 —— 图片缩到 128px 后用 Celebi 量化器（Wu 聚类 + 均值漂移）聚成
///      128 个代表色；
///   2. 打分 —— Score 按「相邻 30° 色相区间的像素占比 × 彩度」排序，挑出最
///      适合当主题源色的那一个：占比大但几乎无彩度的灰、以及只占几个像素的
///      高饱和杂点，都会被它筛掉；
///   3. 展开 —— 源色送进 HCT 色彩空间（CAM16 色相/彩度 + L* 明度）生成色调盘，
///      也就是同色相同彩度、只差明度的一串颜色，再按面板四个色位各取一档。
///
/// 🔴 明度档位照着面板内置预设的观感定，不是照抄 Material 的角色表：
///   - 卡片始终比页面亮一档（浅色 #f5f5f5→#ffffff，深色 #141414→#1d1d1d），
///     这是面板「无边框，层次只用底色深浅表达」的前提；
///   - 深色档的主色取**亮**档（tone 80）——面板的主按钮是反色块，
///     文字用的是底色 `--el-bg-color`，亮主色配深字才读得出来（见 theme.css
///     的「反色选中块」一节）。

export interface MonetPalette {
  primary: string;
  bg_page: string;
  bg_card: string;
  text: string;
}

export interface MonetResult {
  colors: MonetPalette;
  /// 图里没有够彩的颜色（黑白照片、灰度壁纸），配色退回中性色
  achromatic: boolean;
}

/// 取色前的采样边长。量化开销与像素数成正比，原图最长边可达 2560（约 650 万
/// 像素），直接喂进去要跑上百万次聚类；缩到 128 后色调统计不变，量化从秒级降到
/// 毫秒级。这个值也是 material-color-utilities 自带示例用的值。
const SAMPLE_EDGE = 128;

/// 页面/卡片/文字共用的彩度，以及深浅两套明度档。
///
/// 🔴 为什么不能沿用 n1（彩度 4）：
///   - 浅色档：tone 99 已经在 sRGB 色域的最顶上，那里容不下任何彩度，色相会被
///     整个挤掉。实测色相 350 / 20 / 250 三种、彩度 4 / 8 / 16 / 32 四档，
///     tone 99 一律得到 #fffbff —— 换任何背景图「卡片底色」都停在近纯白，
///     莫奈取色对它等于没生效（页面底色的 tone 95 还没顶到色域边界，所以它
///     一直是好的，卡片却不是）。把彩度提到 8、明度往下让出两档（95/99 →
///     94/97），色相才留得住，而「卡片比页面亮一档」的层次不变。
///   - 深色档：tone 6/12/90 落在色域最暗处，别处够用的彩度在这里显得很弱。
///     实测（hue 283 的蓝紫图）：请求彩度 4 → 页面底色 #131316 对深色预设的
///     #141414 只差 2 个色阶、卡片 #1f1f23 对 #1d1d1d 差 6 个；提到 8 也只到
///     差 6 / 10，实机看仍旧「像没变」。所以深色档用一个更大的请求彩度。
///
/// 🔴 两边请求值不同，是色域余量不同导致的：浅色档的 tone 94/97 已经贴着
/// 色域上边界，请求彩度超过 8 就被裁掉（B 卡在 255，实测实际彩度封顶 ~8）；
/// 深色档的 tone 6/12 离边界还远，请求多少就实得多少（16 → 实际 16）。
/// 换言之浅色档已经是它的上限，深色档要更多彩度才追到同等的可见度。
const SURFACE_CHROMA = 8;
/// 深色档专用（见上）。实测 16 时 hue 283 得到 页面 #111223 / 卡片 #1d1e30 /
/// 文字 #e1e0f9，对纯灰分别差 15 / 19 / 17 个色阶 —— 一眼能看出是蓝紫调，
/// 又还没到「染色过头」的程度。
const SURFACE_CHROMA_DARK = 16;
const SURFACE_TONE_PAGE = 94;
const SURFACE_TONE_CARD = 97;

/// Sentry for "image has no usable accent color".
///
/// Score 在**所有**颜色都被筛掉时会退回它内置的兜底色（Google Blue）——对着
/// 一张黑白壁纸给出一个蓝色主色是错的，所以传一个自己认得出来的哨兵色进去，
/// 用它来识别这种情况。#010203 是量化器不可能输出的值（它只会输出图片里真实
/// 出现过的颜色，且这三分量都取到 1/255 量级的概率约等于零）。
const ACHROMATIC_SENTINEL = 0xff010203;

export async function monetFromImage(dataUrl: string, dark: boolean): Promise<MonetResult> {
  // 动态引入：这套算法（量化器 + HCT + 几个 scheme）只在点取色时才用得上，
  // 不该压在首屏体积里；manualChunks 里给它单独分了 monet chunk。
  const { CorePalette, QuantizerCelebi, Score, TonalPalette, argbFromRgb, hexFromArgb } =
    await import("@material/material-color-utilities");

  const data = await samplePixels(dataUrl);
  const pixels: number[] = [];
  for (let i = 0; i < data.length; i += 4) {
    // 半透明像素不参与：量化出来的是它与下层底色混合之后的颜色，不是图本身的色
    if (data[i + 3] < 255) continue;
    pixels.push(argbFromRgb(data[i], data[i + 1], data[i + 2]));
  }
  if (!pixels.length) throw new Error("这张图没有可用的像素");

  const ranked = Score.score(QuantizerCelebi.quantize(pixels, 128), {
    fallbackColorARGB: ACHROMATIC_SENTINEL,
  });
  const source = ranked[0];
  const achromatic = source === ACHROMATIC_SENTINEL;
  const palette = CorePalette.of(source);
  const hex = (argb: number) => hexFromArgb(argb).toLowerCase();

  // 中性色盘：与源色同色相、彩度压到 4 的一支。它只用来取色相（以及灰度图
  // 时的主色），底色与文字都另走 surface 那支提过彩度的（见 SURFACE_CHROMA）。
  const n = palette.n1;

  // 页面/卡片底色与文字色统一出一支色板：与 n 同色相，彩度按深浅各取一档
  // （浅色档受色域所限只能到 8，深色档要走 16，理由见 SURFACE_CHROMA）。
  // 灰度图没有色相可依，退回零彩度，得到一套干净的中性主题。
  const surface = TonalPalette.fromHueAndChroma(
    n.hue,
    achromatic ? 0 : dark ? SURFACE_CHROMA_DARK : SURFACE_CHROMA,
  );
  const bgPage = hex(surface.tone(dark ? 6 : SURFACE_TONE_PAGE));
  const bgCard = hex(surface.tone(dark ? 12 : SURFACE_TONE_CARD));
  const text = hex(surface.tone(dark ? 90 : 10));

  if (achromatic) {
    // 灰度图没有能当主色的颜色。主色退回面板自己的黑白灰口径（浅色近黑、
    // 深色近白），底色仍按这张图展开 —— 得到一套干净的中性主题，
    // 而不是凭空冒出来的蓝色。
    return {
      achromatic: true,
      colors: {
        primary: hex(n.tone(dark ? 90 : 10)),
        bg_page: bgPage,
        bg_card: bgCard,
        text,
      },
    };
  }

  return {
    achromatic: false,
    colors: {
      // 主色取 Material 的常规档位：浅色 40（够深，反色块上的浅字压得住）、
      // 深色 80（够亮，深字压得住）
      primary: hex(palette.a1.tone(dark ? 80 : 40)),
      bg_page: bgPage,
      bg_card: bgCard,
      text,
    },
  };
}

/// 把图缩到 SAMPLE_EDGE 再读出像素。
///
/// 用 canvas 缩放而不是直接量化原图：`getImageData` 的返回体积、量化器的
/// 输入规模都跟像素数走，缩一次把两个都降到可忽略。imageSmoothingQuality
/// 拉满，避免最近邻缩图把高频噪点当成长尾颜色留下来。
async function samplePixels(dataUrl: string): Promise<Uint8ClampedArray> {
  const img = await loadImage(dataUrl);
  const w = img.naturalWidth;
  const h = img.naturalHeight;
  if (!w || !h) throw new Error("图片无法解码");
  const scale = Math.min(1, SAMPLE_EDGE / Math.max(w, h));
  const canvas = document.createElement("canvas");
  canvas.width = Math.max(1, Math.round(w * scale));
  canvas.height = Math.max(1, Math.round(h * scale));
  const ctx = canvas.getContext("2d", { willReadFrequently: true });
  if (!ctx) throw new Error("当前浏览器不支持取色");
  ctx.imageSmoothingQuality = "high";
  ctx.drawImage(img, 0, 0, canvas.width, canvas.height);
  return ctx.getImageData(0, 0, canvas.width, canvas.height).data;
}

function loadImage(src: string): Promise<HTMLImageElement> {
  return new Promise((resolve, reject) => {
    const img = new Image();
    img.onload = () => resolve(img);
    img.onerror = () => reject(new Error("图片无法解码"));
    img.src = src;
  });
}
