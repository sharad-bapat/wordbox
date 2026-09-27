"""Make the demo's sample PDFs. All content is made up.

  letter.pdf   a one-page letter: heading, paragraphs, and a two-column section
  rotated.pdf  a page turned by /Rotate 90 with a CropBox margin, and a line of slanted text
  form.pdf     a filled text field and a free-text note (annotation text, flagged)
  garbled.pdf  the letter with its text layer replaced by private-use characters

usage: python tools/samples.py <out dir>
"""
import os, sys

import fitz

sys.path.insert(0, os.path.dirname(__file__))
import garble

out = sys.argv[1]
os.makedirs(out, exist_ok=True)

LINES = [
    'Dear Sam,',
    '',
    'Thank you for the draft of the garden plan. The layout works well, and the',
    'north bed gets enough light for the herbs you suggested. Two small changes:',
    'move the bench to the east wall, and leave a wider path by the shed.',
    '',
    'The quote for the paving is fine. Please go ahead with the order on Monday.',
]
LEFT = ['Plants', 'Rosemary, thyme', 'Mint, in a pot', 'Lavender by the path']
RIGHT = ['Materials', 'Gravel, 2 tonnes', 'Edging, 30 metres', 'Bench, oak']


def letter(page):
    page.insert_text((72, 90), 'Garden plan: notes', fontsize=20, fontname='helv')
    page.insert_text((72, 116), '14 March', fontsize=10, fontname='helv')
    y = 160
    for line in LINES:
        if line:
            page.insert_text((72, y), line, fontsize=11, fontname='tiro')
        y += 16
    y += 20
    for i, (a, b) in enumerate(zip(LEFT, RIGHT)):
        font = 'hebo' if i == 0 else 'helv'
        page.insert_text((72, y), a, fontsize=11, fontname=font)
        page.insert_text((320, y), b, fontsize=11, fontname=font)
        y += 16
    page.insert_text((72, y + 30), 'Best wishes, Alex', fontsize=11, fontname='tiro')


doc = fitz.open()
letter(doc.new_page(width=612, height=792))
doc.save(os.path.join(out, 'letter.pdf'))

doc = fitz.open()
p = doc.new_page(width=612, height=792)
letter(p)
p.insert_text((330, 600), 'Checked and approved', fontsize=14, fontname='hebo', rotate=0, morph=(fitz.Point(330, 600), fitz.Matrix(-20)))
p.set_cropbox(fitz.Rect(36, 36, 576, 756))
p.set_rotation(90)
doc.save(os.path.join(out, 'rotated.pdf'))

doc = fitz.open()
p = doc.new_page(width=612, height=792)
p.insert_text((72, 90), 'Delivery form', fontsize=20, fontname='helv')
p.insert_text((72, 140), 'Name', fontsize=11, fontname='helv')
p.insert_text((72, 180), 'Address', fontsize=11, fontname='helv')
for label, value, y in (('name', 'Sam Taylor', 126), ('address', '12 Orchard Lane', 166)):
    w = fitz.Widget()
    w.field_name, w.field_type, w.field_value = label, fitz.PDF_WIDGET_TYPE_TEXT, value
    w.rect = fitz.Rect(150, y, 400, y + 20)
    w.text_fontsize = 11
    p.add_widget(w)
note = p.add_freetext_annot(fitz.Rect(72, 230, 400, 270), 'Leave parcels by the side gate, please.', fontsize=11)
note.update()
doc.save(os.path.join(out, 'form.pdf'))

garble.build(os.path.join(out, 'letter.pdf'), os.path.join(out, 'garbled.pdf'), 'pua')
print('wrote', sorted(os.listdir(out)))
