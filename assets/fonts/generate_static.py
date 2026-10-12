"""Regenerate the bundled static Oxanium weights with FontTools 4.65.0.
Developer-only: pip install fonttools==4.65.0. Not used by DAT at build/runtime.
"""
from pathlib import Path
from fontTools.ttLib import TTFont
from fontTools.varLib.instancer import instantiateVariableFont
root = Path(__file__).parent
for weight, style in [(400, 'Regular'), (500, 'Medium'), (600, 'SemiBold'), (700, 'Bold')]:
    font = instantiateVariableFont(TTFont(root / 'Oxanium-Variable.ttf'),
                                   {'wght': weight}, inplace=True, updateFontNames=True)
    font.save(root / f'Oxanium-{style}.ttf')
