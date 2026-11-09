// What the AI agent says and asks, per sector. Edit this file to change the scripts.

class Sector {
  final String id;
  final String label;
  final String defaultCompany;
  final String offer;
  final List<String> questions;
  const Sector(this.id, this.label, this.defaultCompany, this.offer, this.questions);
}

const List<Sector> sectors = [
  Sector(
    'chef',
    'Chef booking (Chefpy)',
    'Chefpy',
    'Chefpy sends verified professional chefs to homes and events: house parties, birthdays, '
        'weddings, one-time chef visits, daily home cooks and corporate catering, in 15+ cities. '
        'All chefs are background-verified and many have 15+ years experience. 5,000+ happy customers, '
        '4.9/5 rating. The menu is fully customisable. Payment by UPI, card, bank transfer or cash.',
    [
      'What occasion do they need a chef for?',
      'How many bookings do they need: one event, several events, or a daily cook?',
      'How many guests or people will be served?',
      'Which date, and is it lunch or dinner?',
      'Which city and area?',
      'Veg, non-veg or both?',
    ],
  ),
  Sector(
    'realestate',
    'Real estate',
    '',
    'We help buyers find and book verified residential properties (apartments, villas and plots) '
        'and arrange site visits at their convenience.',
    [
      'Are they buying to live in or as an investment?',
      'Which type of property: apartment, villa or plot, and what size (BHK or sq ft)?',
      'Which location or area do they prefer?',
      'What is their budget range?',
      'When are they planning to buy?',
      'Which day suits them for a site visit?',
    ],
  ),
  Sector(
    'salon',
    'Salon / clinic / wellness',
    '',
    'We offer professional beauty, skin, hair and wellness services by trained experts, with easy appointment booking.',
    [
      'Which service are they interested in?',
      'Is this their first visit or are they a regular customer?',
      'How many people will come?',
      'Which date and time suits them?',
      'Which branch or area is convenient for them?',
      'Do they have a budget in mind?',
    ],
  ),
  Sector(
    'insurance',
    'Insurance',
    '',
    'We help families choose the right health, life, term and vehicle insurance, and support them during claims.',
    [
      'Which type of insurance are they looking for?',
      'How many family members need cover?',
      'What is the age of the eldest member to be covered?',
      'Do they already have a policy?',
      'What yearly budget do they have in mind?',
      'When can our advisor call them for a detailed discussion?',
    ],
  ),
  Sector('custom', 'Custom (write your own)', '', '', []),
];

class CallRequest {
  final Sector sector;
  final String company;
  final String offer;
  final List<String> questions;
  final String customerName;
  final String number; // E.164
  final String city;
  final String language; // 'hinglish' | 'english'
  final String agentName;
  final bool maleVoice;
  final String model;
  final int maxSeconds;

  const CallRequest({
    required this.sector,
    required this.company,
    required this.offer,
    required this.questions,
    required this.customerName,
    required this.number,
    required this.city,
    required this.language,
    required this.agentName,
    required this.maleVoice,
    required this.model,
    required this.maxSeconds,
  });
}

/// Accepts 98765 43210, 098765..., 9198765..., +9198765... and returns +91XXXXXXXXXX.
/// Returns null when the input does not look like a phone number.
String? normalisePhone(String raw) {
  final cleaned = raw.replaceAll(RegExp(r'[^\d+]'), '');
  if (cleaned.startsWith('+')) {
    final digits = cleaned.substring(1);
    return (digits.length >= 8 && digits.length <= 15 && !digits.contains('+')) ? cleaned : null;
  }
  final d = cleaned.replaceFirst(RegExp(r'^0+'), '');
  if (d.length == 10) return '+91$d';
  if (d.length == 12 && d.startsWith('91')) return '+$d';
  return null;
}

String voiceIdFor(String language, bool male) {
  if (language == 'english') return male ? 'en-IN-PrabhatNeural' : 'en-IN-NeerjaNeural';
  return male ? 'hi-IN-MadhurNeural' : 'hi-IN-SwaraNeural';
}

String firstMessageFor(CallRequest r) {
  final name = r.customerName.trim();
  if (r.language == 'english') {
    return name.isNotEmpty
        ? 'Hello! Am I speaking with $name?'
        : 'Hello! This is ${r.agentName} calling from ${r.company}. Do you have a minute?';
  }
  final verb = r.maleVoice ? 'रहा' : 'रही';
  return name.isNotEmpty
      ? 'नमस्ते! क्या मेरी बात $name जी से हो रही है?'
      : 'नमस्ते! मैं ${r.agentName} बोल $verb हूँ ${r.company} से। क्या आपके पास एक मिनट है?';
}

String buildSystemPrompt(CallRequest r) {
  final who = r.customerName.trim().isEmpty ? 'the customer' : r.customerName.trim();
  final where = r.city.trim().isEmpty ? '' : ' from ${r.city.trim()}';
  final qs = r.questions.where((q) => q.trim().isNotEmpty).map((q) => '   - ${q.trim()}').join('\n');
  final gender = r.maleVoice ? 'male' : 'female';

  final languageRules = r.language == 'english'
      ? '- Speak clear, friendly Indian English. If the customer prefers Hindi, switch to simple Hindi.'
      : '- Speak natural Hinglish like a friendly Indian salesperson. If the customer speaks only English, switch to English.\n'
          '- IMPORTANT FOR THE VOICE ENGINE: write Hindi words in Devanagari script and English words '
          '(booking, party, guests, budget, WhatsApp, quote, site visit, appointment) in English letters. '
          'Example: "नमस्ते! मैं ${r.agentName} बोल ${r.maleVoice ? 'रहा' : 'रही'} हूँ ${r.company} से।"\n'
          '- You are $gender: use ${r.maleVoice ? 'masculine Hindi verb forms (बोल रहा हूँ, करूँगा)' : 'feminine Hindi verb forms (बोल रही हूँ, करूँगी)'}.';

  return '''You are ${r.agentName}, a warm and confident $gender sales executive calling from ${r.company}.
About ${r.company}: ${r.offer}

You are on a LIVE PHONE CALL with $who$where, who enquired earlier.

YOUR GOAL (finish in about 60-90 seconds):
1. Confirm you are talking to $who and ask if they have one minute.
2. Ask these questions ONE AT A TIME, in this order, skipping any they already answered:
$qs
3. Repeat the key numbers back to confirm (for example "so 25 guests, right?").
4. Give a one-line pitch, then close: the team will send details and a quote on WhatsApp.
   Confirm this number is on WhatsApp, thank them, and end the call.

HOW TO TALK:
$languageRules
- Keep every reply to ONE or TWO short sentences, then stop and listen.
- Be warm and helpful, never pushy. Use their name once or twice, not every line.

RULES:
- Never quote an exact price, discount, refund or guarantee. Say the team will send the exact quote on WhatsApp.
- Never invent facts about ${r.company} that are not written above; say the team will confirm.
- If they say "not interested" twice, thank them politely and end the call.
- If they are busy, ask for a good time to call back, then end the call.
- If asked whether you are an AI, say honestly that you are ${r.company}'s AI assistant and offer a call from the team.
- Never ask for card numbers, OTPs or bank details.
- When the conversation is complete, say a short thank-you and use the endCall function to hang up.''';
}

const Map<String, dynamic> leadSchema = {
  'type': 'object',
  'properties': {
    'call_outcome': {
      'type': 'string',
      'enum': ['interested', 'booked', 'callback', 'not_interested', 'wrong_number', 'busy', 'no_conversation'],
    },
    'lead_temperature': {
      'type': 'string',
      'enum': ['hot', 'warm', 'cold'],
    },
    'requirement': {'type': 'string', 'description': 'What the customer wants, in one short line'},
    'bookings_needed': {'type': 'number', 'description': 'How many bookings, units, events or appointments they need'},
    'people_count': {'type': 'number', 'description': 'How many guests, people or family members'},
    'preferred_date': {'type': 'string'},
    'city_area': {'type': 'string'},
    'budget': {'type': 'string'},
    'whatsapp_confirmed': {'type': 'boolean'},
    'callback_time': {'type': 'string'},
    'objections': {'type': 'string'},
    'next_step': {'type': 'string'},
  },
};

const Map<String, String> leadLabels = {
  'call_outcome': 'Outcome',
  'lead_temperature': 'Lead',
  'requirement': 'Requirement',
  'bookings_needed': 'Bookings needed',
  'people_count': 'Guests / people',
  'preferred_date': 'Date',
  'city_area': 'City / area',
  'budget': 'Budget',
  'whatsapp_confirmed': 'WhatsApp confirmed',
  'callback_time': 'Call back at',
  'objections': 'Objections',
  'next_step': 'Next step',
};

Map<String, dynamic> buildAssistant(CallRequest r) => {
      'name': '${r.company} - ${r.agentName}'.length > 40
          ? '${r.company} - ${r.agentName}'.substring(0, 40)
          : '${r.company} - ${r.agentName}',
      'firstMessage': firstMessageFor(r),
      'model': {
        'provider': 'openai',
        'model': r.model,
        'temperature': 0.5,
        'messages': [
          {'role': 'system', 'content': buildSystemPrompt(r)},
        ],
      },
      'voice': {'provider': 'azure', 'voiceId': voiceIdFor(r.language, r.maleVoice)},
      // Deepgram Nova-3 "multi" understands Hindi and English mixed in one sentence
      'transcriber': {'provider': 'deepgram', 'model': 'nova-3', 'language': 'multi'},
      'endCallFunctionEnabled': true,
      'endCallMessage': r.language == 'english' ? 'Thank you! Have a great day.' : 'धन्यवाद! आपका दिन शुभ हो।',
      'maxDurationSeconds': r.maxSeconds,
      'silenceTimeoutSeconds': 20,
      'artifactPlan': {'recordingEnabled': true},
      'analysisPlan': {
        'summaryPlan': {'enabled': true},
        'structuredDataPlan': {'enabled': true, 'schema': leadSchema},
      },
    };

Map<String, dynamic> buildCallBody(CallRequest r, String phoneNumberId) {
  final name = r.customerName.trim();
  return {
    'phoneNumberId': phoneNumberId,
    'customer': {
      'number': r.number,
      if (name.isNotEmpty) 'name': name.length > 40 ? name.substring(0, 40) : name,
    },
    'assistant': buildAssistant(r),
    'metadata': {'source': 'ai-caller-app', 'sector': r.sector.id, 'company': r.company},
  };
}
