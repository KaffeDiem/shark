# Editor typography

The file editor shows code at **14px with 1.5× line height (21px rows)**,
adjustable from 10 to 24px with `cmd-=`/`cmd--` (Reset editor text size in `cmd-k`).
The diff uses **13px text on the same 21px rows**, including inline editing. Its text
scales at 13/14 of the editor size with the same zoom controls and saved setting.
The smaller diff text leaves more space around each line.

The constants, `code_font`, and `diff_font` are in `src/editor.rs`.

## Why 14px

Reading speed is at its maximum over a wide range of text sizes and falls steeply below a
critical print size. The consensus critical size is an x-height of about 0.2° of visual
angle, and it rises with age.

| Finding | Source |
|---|---|
| Reading speed is at its maximum for x-heights from about 0.2° to 2°. Below the critical print size (0.15–0.3° by individual) it drops sharply. | Legge & Bigelow 2011, *J Vision* 11(5):8, https://doi.org/10.1167/11.5.8 |
| The critical print size rises from about 0.10° (age 23) to 0.135° (age 68) and 0.18° (age 81). | Calabrèse et al. 2016, *IOVS*, n=645, https://doi.org/10.1167/iovs.16-19580 |
| On screen, fixations get shorter as text grows up to about 22pt, and comprehension is worse at 10–12pt than at 18pt. These "pt" were on a 17-inch 1024×768 (about 75 ppi) screen, so each is about 1.45–2.2 Mac logical px: even their 10pt condition is about 14.5–22 Mac px. The direction carries over; the numbers do not. | Rello, Pielot & Marcos, CHI 2016, n=104, https://doi.org/10.1145/2858036.2858204 |
| Glance legibility thresholds rise about 80% from age 20 to 65; larger type reduces them. | Dobres et al. 2016, *Ergonomics*, https://doi.org/10.1080/00140139.2015.1137637 |
| Apple's default macOS text size is 13pt. JetBrains Mono recommends 13. VS Code and Xcode ship 12; Zed ships 15. | Apple HIG Typography; JetBrains Mono site; editor source defaults |

JetBrains Mono's x-height is 0.55 em. At 60 cm on a Mac (109–129 logical ppi), the
x-height at 14px is about 0.15–0.17°. That is close to the 0.2° consensus without giving
up much density. 16px, two `cmd-=` presses away, gives about 0.165–0.196°. Reaching 0.2° takes about 16.5px on a 109-ppi
external display and about 19px on a 129-ppi laptop.

## Why 1.5× line height

| Finding | Source |
|---|---|
| In central (foveal) vision, reading single words flanked by lines above and below improves with spacing up to 1.2–1.5× "standard" spacing, then plateaus. Standard spacing is 2.6× x-height, about 1.43 em for this font, so the plateau starts at about 1.7–2.1 em. That is looser than 1.5, but the measure is single-word RSVP rather than passage reading. In peripheral vision it kept improving. | Chung 2004, *Optom Vis Sci* 81:525, https://doi.org/10.1097/00006324-200407000-00014 |
| Very tight spacing (about 0.96× the font size) hurt comprehension. About 2.16× was rated worse than the default. | Rello et al. 2016 (above) |
| In proofreading (error detection), 1.5 spacing balanced speed against errors found. | Chan, Tsang & Ng 2014, *Human Factors*, https://doi.org/10.1177/0018720813499368 |
| Some readers lose their place when lines are close together. WCAG asks for at least 1.5× line spacing (AAA), and that content still work at 1.5× (AA). | WCAG 2.2 SC 1.4.8 and 1.4.12 |
| Code is read less linearly than prose: 30–40% of eye movements jump between lines. | Busjahn et al., ICPC 2015 |
| Effective editor line heights: VS Code 1.5 on macOS, JetBrains about 1.6, Zed 1.618. | `fontInfo.ts`, `EditorView.java`, Zed `default.json` |

1.5× is a practical default, not a measured optimum. Very tight spacing clearly hurts, and
gains elsewhere are small and depend on the task. Looser leading costs visible lines: in a
900px-tall editor, 21px rows show 43 lines and 24px rows show 37. Code navigation already
takes a large share of developers' time (Short & Henley 2019).

## Limits of the evidence

No controlled study has varied font size or line height for reading or editing code
(Oliveira et al. 2023, *JSS*, a review of 15 human-subject formatting studies). The values
above combine vision-science thresholds, prose studies on screens and editor convention.
Effects of line spacing are small outside the extremes. Size matters more than spacing,
especially for older readers. That is why the size is adjustable and the line-height ratio
is fixed.
