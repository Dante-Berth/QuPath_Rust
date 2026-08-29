# Build the preprint. `make` runs the full bibtex cycle; `make quick` is one
# pass with no bibliography, for checking that an edit still compiles.
main.pdf: main.tex ../paper.bib
	pdflatex -interaction=nonstopmode -halt-on-error main.tex
	bibtex main
	pdflatex -interaction=nonstopmode main.tex
	pdflatex -interaction=nonstopmode main.tex

.PHONY: quick clean
quick:
	pdflatex -interaction=nonstopmode -halt-on-error main.tex

clean:
	rm -f main.pdf main.aux main.bbl main.blg main.log main.out
