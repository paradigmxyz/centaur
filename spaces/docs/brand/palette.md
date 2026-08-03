# MagikDev Spaces — primary palette

Source: MagikDev brand guide (locked for product UI / marketing surfaces).

| Name | HEX | RGB | Use |
|------|-----|-----|-----|
| Signal Red | `#C8102E` | 200 / 16 / 46 | Accent, alerts, CTAs |
| Deep Navy | `#0A2540` | 10 / 37 / 64 | Headlines, primary surface |
| Steel Grey | `#5C6B7A` | 92 / 107 / 122 | Body text, secondary |
| Charcoal | `#1F2933` | 31 / 41 / 51 | Body text default |
| Fog | `#F4F6F8` | 244 / 246 / 248 | Backgrounds, panels |
| Paper White | `#FFFFFF` | 255 / 255 / 255 | Default background |
| Amber Accent | `#E8B500` | 232 / 181 / 0 | Sparingly: highlights only |

## Color ratios (60 / 30 / 10)

- **60%** Paper White or Fog — let the page breathe
- **30%** Deep Navy — typography and structural elements
- **10%** Signal Red — one accent moment per page; never a wall of red

## CSS variables (suggested)

```css
:root {
  --color-signal-red: #c8102e;
  --color-deep-navy: #0a2540;
  --color-steel-grey: #5c6b7a;
  --color-charcoal: #1f2933;
  --color-fog: #f4f6f8;
  --color-paper-white: #ffffff;
  --color-amber-accent: #e8b500;
}
```

## Logos

Master PNG lockups under [`assets/`](assets/):

| File | Use |
|------|-----|
| [`MagikDev-Colour.png`](assets/MagikDev-Colour.png) | Default / full-color on light backgrounds |
| [`MagikDev-Grey.png`](assets/MagikDev-Grey.png) | Monochrome / secondary surfaces |
| [`MagikDev-White.png`](assets/MagikDev-White.png) | Reverse on Deep Navy or dark surfaces |

Prefer SVG later for UI scaling; these PNGs are the current source of truth.
Do not commit secrets or unrelated binaries here.
