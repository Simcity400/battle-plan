# Hold content-fitted table widths during playback

Changing rows can move the cells a reader is following. Each table placement keeps its measured column widths while playing. Wider values grow the affected fit through the usual layout, which also assigns any spare space. The header and canvas use that same layout.

Refit to the current sample on pause, seek, resize, or a settings or column change. The rule affects layout only. Recorded values, display formatting and missing-data reasons keep their existing meaning.

[Table lane layout](../../ui/src/families/table/laneView.ts)
