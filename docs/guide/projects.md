# Projects

A study is fifty slides, not one. A QuPath project is a directory that keeps
them together with the metadata you have attached to each.

```bash
cargo run --release -p qupath-app -- study/project.qpproj
```

Or **File → Open project…**, or **p**. A project opens and *lists* its images
without opening any of them, which is what a project is for.

## What the browser gives you

| | |
| --- | --- |
| **Thumbnails** | One per entry, from the slide itself. |
| **Sort** | By name, by file name, or by any metadata key in the project. |
| **Search** | Free text, or `key=value` against the metadata. |
| **Metadata editing** | Right-click an entry. |
| **Name masking** | A checkbox, for blinded reading. |

Double-click an entry to open that slide in the viewer.

### Searching

The search box matches QuPath's `ProjectEntryPredicate`: the metadata map is
flattened to `key=value` tokens and each is matched with **`contains`**, not
equality. So `stain=h` matches `stain=h&e`, and a bare `stain` matches every
entry that has the key at all.

### Masking names for blinded reading

Ticking **mask image names** replaces every displayed name with the entry's
`randomizedName` and shuffles the rows, as QuPath does.

It goes further than QuPath in one place, deliberately. A name leaks out of a
browser in more than one spot, and copying QuPath's two lines would leave three
of them open:

- the **search box** would still match real names — typing `tumour` answers
  "which row is that?" with a list of one;
- the **hover** would show the slide's full path, and a study's file names are
  its names;
- **sorting** by file name or by a metadata key puts the rows in a ranking of
  the very thing being hidden. The rows stay anonymous; their *sequence* does
  not.

All three are closed here. The third is a documented divergence: while masking
is on, the sort menu **drops** the choices that would order rows by a hidden
value, and drops them again if such an order was picked before the box was
ticked. QuPath offers them regardless, which makes its mask liftable by a
single menu choice.

**Thumbnails are deliberately not masked**, as in QuPath. The mask is against
recognising a *name*, not against recognising a slide — a pane of grey squares
is one nothing can be done with. The hover says so, rather than letting a tick
imply more than it delivers.

## Creating a project

**File → New project…** writes a directory in QuPath's own layout:

```text
project.qpproj          JSON: {version, uri, images: [...]}
data/<entry-id>/
    thumbnail.jpg
    server.json
    summary.json
```

Projects written here **open in QuPath**. That is checked by a recorded test
that runs QuPath's own `ProjectIO.loadProject` against the output, rather than
by reading QuPath's source and believing it — four separate defects were found
that way, each hiding the next, and each producing a file that parsed
perfectly.

## What is deliberately not written

**`data.qpdata` — the objects.** That is Java serialisation tied to Java's
object graph. Guessing at it is how a project becomes corrupt in a way you
discover months later. Detections are written as **GeoJSON alongside** instead;
see [Exporting to QuPath](exporting.md).

**The stored image metadata block** — the level table, tile sizes, channels and
pixel calibration. This one is not a limitation but a decision, and the
reasoning is worth knowing because it affects your data:

QuPath's `AbstractServerBuilder.build` calls `setMetadata` with that block when
the image is opened, so the block **replaces** what OpenSlide would have read.
A block that goes stale — the file replaced, re-scanned at a different
resolution, swapped for a re-export — is the wrong level table applied to the
right path, with no reader anywhere to flag it.

The block is now known to be *safe* to write: a recorded comparison against
QuPath's own OpenSlide reader agrees in every field — level count, every width,
height and downsample, the preferred tile size, the pixel size. It is still not
written, because the two sides of the trade are not the same kind of thing.
Writing it saves one OpenSlide open per entry on a project's first open.
Omitting it costs those milliseconds and cannot silently corrupt an analysis.

The practical consequence for you: **QuPath opens the slide itself** and reads
its own level table, exactly as it does for projects QuPath wrote.

## One trap, if you write project files yourself

Entry URIs must be **absolute**. `file://data/CMU-1.svs` is not a bare path —
it is a well-formed URI naming the *host* `data` and the path `/CMU-1.svs`, so
QuPath looks for the slide on another machine and the entry fails to open. The
application resolves against the working directory, which is what Java's
`File.toURI()` does.
