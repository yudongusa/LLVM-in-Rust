.PHONY: update-baselines codegen-quality-gate

codegen-quality-gate:
	python3 scripts/codegen_quality_gate.py

update-baselines:
	python3 scripts/codegen_quality_gate.py --update-baselines --bless
