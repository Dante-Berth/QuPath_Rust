# Build the preprint. `make` runs the full bibtex cycle; `make quick` is one
# pass with no bibliography, for checking that an edit still compiles.
#
# Figures are converted from the SVGs in docs/benchmarks/, which are the
# tracked source — the PDFs here are build products and are gitignored. Do not
# regenerate the SVGs to make a figure for the paper; they are recordings.
FIGDIR  := figures
FIGS    := $(FIGDIR)/compare-endtoend.pdf $(FIGDIR)/compare-threads.pdf
BENCH   := ../../docs/benchmarks

main.pdf: main.tex ../paper.bib $(FIGS)
	pdflatex -interaction=nonstopmode -halt-on-error main.tex
	bibtex main
	pdflatex -interaction=nonstopmode main.tex
	pdflatex -interaction=nonstopmode main.tex

$(FIGDIR)/%.pdf: $(BENCH)/%.svg | $(FIGDIR)
	cairosvg $< -o $@

$(FIGDIR):
	mkdir -p $(FIGDIR)

.PHONY: quick clean
quick: $(FIGS)
	pdflatex -interaction=nonstopmode -halt-on-error main.tex

clean:
	rm -f main.pdf main.aux main.bbl main.blg main.log main.out
	rm -rf $(FIGDIR)
