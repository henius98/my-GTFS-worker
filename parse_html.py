import sys
from bs4 import BeautifulSoup

with open('/home/henius/.gemini/code/brain/b099adff-54cf-49a7-963e-f7af50abfd89/.system_generated/steps/203/content.md', 'r') as f:
    html = f.read().split('---', 1)[-1]

soup = BeautifulSoup(html, 'html.parser')
for h2 in soup.find_all('h2'):
    print(f"\n# {h2.text}")
    elem = h2.find_next_sibling()
    while elem and elem.name not in ['h2', 'h1']:
        if elem.name == 'p':
            print(elem.text)
        elif elem.name == 'pre':
            print("```\n" + elem.text + "\n```")
        elif elem.name == 'ul':
            for li in elem.find_all('li'):
                print(f"- {li.text}")
        elem = elem.find_next_sibling()
