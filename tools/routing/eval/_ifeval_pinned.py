# coding=utf-8
# Copyright 2026 The Google Research Authors.
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#     http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.

# Selected check_following methods from immutable revision
# e6890f85757dd84e27ca6df2dd30651dafad28e0. Apache-2.0 notice retained.
# build_description/random defaults and optional NLP dependencies are excluded.
import collections
import json
import re
from types import SimpleNamespace

class Instruction:
    def __init__(self, instruction_id): self.id = instruction_id

# Exact regex used by upstream NLTK RegexpTokenizer, without NLTK/data downloads.
instructions_util = SimpleNamespace(count_words=lambda text: len(re.findall(r"\w+", text)))
CONSTRAINED_RESPONSES = ('My answer is yes.', 'My answer is no.', 'My answer is maybe.')
_COMPARISON_RELATION = ('less than', 'at least')

class PlaceholderChecker(Instruction):

    def check_following(self, value):
        placeholders = re.findall('\\[.*?\\]', value)
        num_placeholders = len(placeholders)
        return num_placeholders >= self._num_placeholders

class BulletListChecker(Instruction):

    def check_following(self, value):
        bullet_lists = re.findall('^\\s*\\*[^\\*].*$', value, flags=re.MULTILINE)
        bullet_lists_2 = re.findall('^\\s*-.*$', value, flags=re.MULTILINE)
        num_bullet_lists = len(bullet_lists) + len(bullet_lists_2)
        return num_bullet_lists == self._num_bullets

class ConstrainedResponseChecker(Instruction):

    def check_following(self, value):
        value = value.strip()
        for constrained_response in self._constrained_responses:
            if constrained_response in value:
                return True
        return False

class HighlightSectionChecker(Instruction):

    def check_following(self, value):
        num_highlights = 0
        highlights = re.findall('\\*[^\\n\\*]*\\*', value)
        double_highlights = re.findall('\\*\\*[^\\n\\*]*\\*\\*', value)
        for highlight in highlights:
            if highlight.strip('*').strip():
                num_highlights += 1
        for highlight in double_highlights:
            if highlight.removeprefix('**').removesuffix('**').strip():
                num_highlights += 1
        return num_highlights >= self._num_highlights

class SectionChecker(Instruction):

    def check_following(self, value):
        section_splitter_patten = '\\s?' + self._section_spliter + '\\s?\\d+\\s?'
        sections = re.split(section_splitter_patten, value)
        num_sections = len(sections) - 1
        return num_sections >= self._num_sections

class ParagraphChecker(Instruction):

    def check_following(self, value):
        paragraphs = re.split('\\s?\\*\\*\\*\\s?', value)
        num_paragraphs = len(paragraphs)
        for index, paragraph in enumerate(paragraphs):
            if not paragraph.strip():
                if index == 0 or index == len(paragraphs) - 1:
                    num_paragraphs -= 1
                else:
                    return False
        return num_paragraphs == self._num_paragraphs

class PostscriptChecker(Instruction):

    def check_following(self, value):
        value = value.lower()
        if self._postscript_marker == 'P.P.S':
            postscript_pattern = '\\s*p\\.\\s?p\\.\\s?s.*$'
        elif self._postscript_marker == 'P.S.':
            postscript_pattern = '\\s*p\\.\\s?s\\..*$'
        else:
            postscript_pattern = '\\s*' + self._postscript_marker.lower() + '.*$'
        postscript = re.findall(postscript_pattern, value, flags=re.MULTILINE)
        return True if postscript else False

class KeywordChecker(Instruction):

    def check_following(self, value):
        for keyword in self._keywords:
            if not re.search(keyword, value, flags=re.IGNORECASE):
                return False
        return True

class KeywordFrequencyChecker(Instruction):

    def check_following(self, value):
        actual_occurrences = len(re.findall(self._keyword, value, flags=re.IGNORECASE))
        if self._comparison_relation == _COMPARISON_RELATION[0]:
            return actual_occurrences < self._frequency
        elif self._comparison_relation == _COMPARISON_RELATION[1]:
            return actual_occurrences >= self._frequency

class NumberOfWords(Instruction):

    def check_following(self, value):
        num_words = instructions_util.count_words(value)
        if self._comparison_relation == _COMPARISON_RELATION[0]:
            return num_words < self._num_words
        elif self._comparison_relation == _COMPARISON_RELATION[1]:
            return num_words >= self._num_words

class JsonFormat(Instruction):

    def check_following(self, value):
        value = value.strip().removeprefix('```json').removeprefix('```Json').removeprefix('```JSON').removeprefix('```').removesuffix('```').strip()
        try:
            json.loads(value)
        except ValueError as _:
            return False
        return True

class ParagraphFirstWordCheck(Instruction):

    def check_following(self, value):
        paragraphs = re.split('\\n\\n', value)
        num_paragraphs = len(paragraphs)
        for paragraph in paragraphs:
            if not paragraph.strip():
                num_paragraphs -= 1
        if self._nth_paragraph <= num_paragraphs:
            paragraph = paragraphs[self._nth_paragraph - 1].strip()
            if not paragraph:
                return False
        else:
            return False
        first_word = ''
        punctuation = {'.', ',', '?', '!', "'", '"'}
        word = paragraph.split()[0].strip()
        word = word.lstrip("'")
        word = word.lstrip('"')
        for letter in word:
            if letter in punctuation:
                break
            first_word += letter.lower()
        return num_paragraphs == self._num_paragraphs and first_word == self._first_word

class ForbiddenWords(Instruction):

    def check_following(self, value):
        for word in self._forbidden_words:
            if re.search('\\b' + word + '\\b', value, flags=re.IGNORECASE):
                return False
        return True

class TwoResponsesChecker(Instruction):

    def check_following(self, value):
        valid_responses = list()
        responses = value.split('******')
        for index, response in enumerate(responses):
            if not response.strip():
                if index != 0 and index != len(responses) - 1:
                    return False
            else:
                valid_responses.append(response)
        return len(valid_responses) == 2 and valid_responses[0].strip() != valid_responses[1].strip()

class RepeatPromptThenAnswer(Instruction):

    def check_following(self, value):
        if value.strip().lower().startswith(self._prompt_to_repeat.strip().lower()):
            return True
        return False

class EndChecker(Instruction):

    def check_following(self, value):
        value = value.strip().strip('"').lower()
        self._end_phrase = self._end_phrase.strip().lower()
        return value.endswith(self._end_phrase)

class TitleChecker(Instruction):

    def check_following(self, value):
        pattern = '<<[^\\n]+>>'
        re_pattern = re.compile(pattern)
        titles = re.findall(re_pattern, value)
        for title in titles:
            if title.lstrip('<').rstrip('>').strip():
                return True
        return False

class LetterFrequencyChecker(Instruction):

    def check_following(self, value):
        value = value.lower()
        letters = collections.Counter(value)
        if self._comparison_relation == _COMPARISON_RELATION[0]:
            return letters[self._letter] < self._frequency
        else:
            return letters[self._letter] >= self._frequency

class CommaChecker(Instruction):

    def check_following(self, value):
        return not re.search('\\,', value)

class QuotationChecker(Instruction):

    def check_following(self, value):
        value = value.strip()
        return len(value) > 1 and value[0] == '"' and (value[-1] == '"')

